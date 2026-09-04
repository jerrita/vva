pub mod app;
pub mod command;
pub mod session_manager;
pub mod views;

use std::io::{self, stdout};
use std::sync::Arc;
use std::time::Duration;
use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Terminal;
use tracing::error;

use crate::config::AppConfig;
use crate::storage::SessionStore;
use app::{ActiveModal, App};
use session_manager::{ManagedSession, SessionManager};
use views::chat::render_chat;
use views::header::render_header;
use views::help_modal::render_help_modal;
use views::input_area::render_input_area;
use views::models_modal::render_models_modal;
use views::sessions_modal::render_sessions_modal;
use views::sidebar::render_sidebar;

/// 启动 Ratatui 交互式终端 UI
pub async fn run_tui(
    store: Arc<dyn SessionStore>,
    app_config: Option<AppConfig>,
    initial_session_id: Option<String>,
    workspace: std::path::PathBuf,
    default_model: String,
    default_variant: Option<String>,
    enable_web_search: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let session_mgr = Arc::new(SessionManager::new(store.clone(), app_config, workspace));

    // 加载或创建初始 Session 单例
    let initial_session: Arc<ManagedSession> = if let Some(sid) = initial_session_id {
        match session_mgr.get_or_load(&sid).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to load session {sid}: {e}. Creating new session.");
                session_mgr
                    .create_session(default_model, default_variant, None)
                    .await?
            }
        }
    } else {
        session_mgr
            .create_session(default_model, default_variant, None)
            .await?
    };

    let mut app = App::new(session_mgr, initial_session, enable_web_search).await;

    let res = run_app_loop(&mut terminal, &mut app).await;

    // 恢复终端
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("UI error: {err:?}");
    }

    Ok(())
}

async fn run_app_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    let mut reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        // 读取当前会话的元信息用于渲染
        let (session_id, session_title, session_model, session_variant, session_workspace) = {
            let s = app.active_session.session.read().await;
            (
                s.id.clone(),
                s.title.clone().unwrap_or_default(),
                s.current_model.clone(),
                s.current_variant.clone(),
                s.workspace_root.display().to_string(),
            )
        };
        let status_text = app.active_session.status().await;

        terminal.draw(|frame| {
            let size = frame.area();

            // 宽屏时 (>= 90 宽) 渲染右侧详情边栏；窄屏自动隐藏
            let (main_area, sidebar_area) = if size.width >= 90 {
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(60), Constraint::Length(30)])
                    .split(size);
                (chunks[0], Some(chunks[1]))
            } else {
                (size, None)
            };

            // 主工作区垂直三段切分：顶部极简 Title + 中间对话流 + 底部复合输入区
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // 顶部栏 (仅 Title)
                    Constraint::Min(5),    // 对话区
                    Constraint::Length(5), // 输入区
                ])
                .split(main_area);

            // 1. 顶部栏 (无 branding，仅 Title)
            render_header(app, frame, main_chunks[0], &session_title);

            // 2. 中间对话流
            render_chat(app, frame, main_chunks[1]);

            // 3. 底部输入区 (含上方 Agent/WebSearch 状态、命令浮层、输入框、下方模型状态)
            render_input_area(
                app,
                frame,
                main_chunks[2],
                &session_model,
                session_variant.as_deref(),
                &session_workspace,
                &status_text,
            );

            // 4. 右侧边栏
            if let Some(sidebar_rect) = sidebar_area {
                render_sidebar(
                    app,
                    frame,
                    sidebar_rect,
                    &session_id,
                    &session_workspace,
                    &session_model,
                    session_variant.as_deref(),
                );
            }

            // 5. 模态弹窗层
            match &app.modal {
                ActiveModal::Sessions => render_sessions_modal(app, frame, size),
                ActiveModal::Models => render_models_modal(app, frame, size),
                ActiveModal::Help => render_help_modal(frame, size),
                ActiveModal::None => {}
            }
        })?;

        if app.should_quit {
            break;
        }

        tokio::select! {
            _ = tick_interval.tick() => {
                app.tick();
            }

            event_res = app.event_rx.recv() => {
                if let Ok(event) = event_res {
                    app.handle_agent_event(event).await;
                }
            }

            maybe_event = reader.next() => {
                if let Some(Ok(crossterm_event)) = maybe_event {
                    handle_crossterm_event(app, crossterm_event).await;
                }
            }
        }
    }

    Ok(())
}

async fn handle_crossterm_event(app: &mut App, event: CrosstermEvent) {
    if let CrosstermEvent::Key(key) = event {
        if key.kind != KeyEventKind::Press {
            return;
        }

        // 优先处理模态框交互
        match &app.modal {
            ActiveModal::Sessions => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.modal = ActiveModal::None;
                    }
                    KeyCode::Tab | KeyCode::Char('p') => {
                        let ws = app.session_mgr.workspace().to_path_buf();
                        if let Some(ref mut sm) = app.sessions_modal {
                            sm.toggle_scope(&ws);
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(ref mut sm) = app.sessions_modal {
                            sm.previous();
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(ref mut sm) = app.sessions_modal {
                            sm.next();
                        }
                    }
                    KeyCode::Enter => {
                        let selected_id = app
                            .sessions_modal
                            .as_ref()
                            .and_then(|sm| sm.selected_session().map(|s| s.id.clone()));

                        if let Some(sid) = selected_id {
                            app.modal = ActiveModal::None;
                            let _ = app.switch_to_session(&sid).await;
                        }
                    }
                    _ => {}
                }
                return;
            }
            ActiveModal::Models => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.modal = ActiveModal::None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(ref mut mm) = app.models_modal {
                            mm.previous();
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(ref mut mm) = app.models_modal {
                            mm.next();
                        }
                    }
                    KeyCode::Enter => {
                        let selected_spec = app
                            .models_modal
                            .as_ref()
                            .and_then(|mm| mm.selected_model().map(|m| m.full_id.clone()));

                        if let Some(spec) = selected_spec {
                            app.modal = ActiveModal::None;
                            app.switch_model(&spec).await;
                        }
                    }
                    _ => {}
                }
                return;
            }
            ActiveModal::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                    app.modal = ActiveModal::None;
                }
                return;
            }
            ActiveModal::None => {}
        }

        // 常规输入交互
        // 1. 命令建议选单交互 (Claude Code / OpenCode 风格)
        if !app.matched_commands.is_empty() {
            match key.code {
                KeyCode::Up => {
                    app.command_select_previous();
                    return;
                }
                KeyCode::Down => {
                    app.command_select_next();
                    return;
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.command_select_previous();
                    return;
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.command_select_next();
                    return;
                }
                KeyCode::Tab => {
                    app.complete_selected_command();
                    return;
                }
                KeyCode::Enter => {
                    if let Some(cmd) = app.matched_commands.get(app.selected_command_index) {
                        app.input = cmd.name.to_string();
                    }
                    app.matched_commands.clear();
                    app.selected_command_index = 0;
                    app.submit_input().await;
                    return;
                }
                KeyCode::Esc => {
                    app.matched_commands.clear();
                    app.selected_command_index = 0;
                    return;
                }
                _ => {}
            }
        }

        // 2. 普通按键处理
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if app.active_session.is_running() {
                    app.active_session.abort_current().await;
                    app.chat_entries.push(app::ChatEntry::SystemNotice {
                        text: "Aborted current turn.".into(),
                    });
                } else {
                    app.should_quit = true;
                }
            }
            KeyCode::Enter => {
                app.submit_input().await;
            }
            KeyCode::Tab => {
                if app.input.trim_start().starts_with('/') {
                    app.update_command_matches();
                    if !app.matched_commands.is_empty() {
                        app.complete_selected_command();
                    }
                }
            }
            KeyCode::Char(c) => {
                app.insert_char(c);
            }
            KeyCode::Backspace => {
                app.backspace();
            }
            KeyCode::Delete => {
                app.delete();
            }
            KeyCode::Left => {
                app.move_cursor_left();
            }
            KeyCode::Right => {
                app.move_cursor_right();
            }
            KeyCode::Home => {
                app.move_cursor_home();
            }
            KeyCode::End => {
                app.move_cursor_end();
            }
            KeyCode::PageUp => {
                app.auto_scroll = false;
                app.scroll_offset = app.scroll_offset.saturating_add(5);
            }
            KeyCode::PageDown => {
                if app.scroll_offset > 5 {
                    app.scroll_offset -= 5;
                } else {
                    app.scroll_offset = 0;
                    app.auto_scroll = true;
                }
            }
            KeyCode::Esc if !app.input.is_empty() => {
                app.input.clear();
                app.cursor_pos = 0;
                app.update_command_matches();
            }
            _ => {}
        }
    }
}
