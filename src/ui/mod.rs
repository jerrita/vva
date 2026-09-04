pub mod app;
pub mod command;
pub mod history;
pub mod input;
pub mod markdown;
pub mod modals;
pub mod session_manager;
pub mod views;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CrosstermEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use std::io::{self, stdout};
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

use crate::config::AppConfig;
use crate::storage::SessionStore;
use app::{ActiveModal, App};
use session_manager::{ManagedSession, SessionManager};
use views::chat::render_chat;
use views::config_modal::render_config_modal;
use views::header::render_header;
use views::help_modal::render_help_modal;
use views::input_area::render_input_area;
use views::models_modal::render_models_modal;
use views::sessions_modal::render_sessions_modal;
use views::sidebar::{render_sidebar, SidebarInfo};
use views::variants_modal::render_variants_modal;

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
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;

    let keyboard_enhancement = supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhancement {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
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
    if keyboard_enhancement {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
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

    let (
        mut session_id,
        mut session_title,
        mut session_model,
        mut session_variant,
        mut session_workspace,
    ) = {
        let s = app.active_session.session.read().await;
        (
            s.id.clone(),
            s.title.clone().unwrap_or_default(),
            s.current_model.clone(),
            s.current_variant.clone(),
            s.workspace_root.display().to_string(),
        )
    };

    loop {
        // 读取当前会话的元信息用于渲染（使用 try_read 避免在后台执行期间阻塞 UI 事件主循环）
        if let Ok(s) = app.active_session.session.try_read() {
            session_id = s.id.clone();
            session_title = s.title.clone().unwrap_or_default();
            session_model = s.current_model.clone();
            session_variant = s.current_variant.clone();
            session_workspace = s.workspace_root.display().to_string();
        }
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

            let input_lines = app.input_line_count();
            // 输入区由 3 部分构成：上方状态栏(1行) + 输入框(边框2行 + 文本行) + 下方状态栏(1行)
            // 文本行数动态自适应，最多不超过 (main_area.height - 8) / 2（在 1 到 12 行之间）
            let max_text_rows = ((main_area.height.saturating_sub(8)) / 2).clamp(1, 12) as usize;
            let text_rows = input_lines.clamp(1, max_text_rows);
            let input_area_height = (text_rows as u16) + 4;

            // 主工作区垂直三段切分：顶部极简 Title + 中间对话流 + 底部复合输入区
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),                 // 顶部栏 (仅 Title)
                    Constraint::Min(5),                    // 对话区
                    Constraint::Length(input_area_height), // 输入区动态自适应高度
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
                let info = SidebarInfo {
                    session_id: &session_id,
                    session_title: &session_title,
                    workspace: &session_workspace,
                    model: &session_model,
                    variant: session_variant.as_deref(),
                };
                render_sidebar(app, frame, sidebar_rect, &info);
            }

            // 5. 模态弹窗层
            match &app.modal {
                ActiveModal::Sessions => render_sessions_modal(app, frame, size),
                ActiveModal::Models => render_models_modal(app, frame, size),
                ActiveModal::Variants => render_variants_modal(app, frame, size),
                ActiveModal::Config => render_config_modal(app, frame, size),
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
    if let CrosstermEvent::Mouse(mouse) = event {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.scroll_up(3);
            }
            MouseEventKind::ScrollDown => {
                app.scroll_down(3);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if app.modal == ActiveModal::None && app.max_scroll > 0 && app.chat_area.height > 0 {
                    let scrollbar_col = app.chat_area.x + app.chat_area.width.saturating_sub(1);
                    let in_scrollbar_col = mouse.column >= scrollbar_col.saturating_sub(1)
                        && mouse.column <= scrollbar_col;
                    let in_chat_rows = mouse.row >= app.chat_area.y
                        && mouse.row < app.chat_area.y + app.chat_area.height;

                    if in_scrollbar_col && in_chat_rows {
                        app.is_dragging_scrollbar = true;
                        let track_height = app.chat_area.height as usize;
                        let row_offset = (mouse.row as usize).saturating_sub(app.chat_area.y as usize);
                        let ratio = row_offset as f64 / track_height.saturating_sub(1).max(1) as f64;
                        app.scroll_to_ratio(ratio);
                    } else {
                        app.is_dragging_scrollbar = false;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if app.is_dragging_scrollbar && app.max_scroll > 0 && app.chat_area.height > 0 {
                    let track_height = app.chat_area.height as usize;
                    let row_offset = if (mouse.row as usize) < app.chat_area.y as usize {
                        0
                    } else {
                        (mouse.row as usize - app.chat_area.y as usize).min(track_height.saturating_sub(1))
                    };
                    let ratio = row_offset as f64 / track_height.saturating_sub(1).max(1) as f64;
                    app.scroll_to_ratio(ratio);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                app.is_dragging_scrollbar = false;
            }
            _ => {}
        }
        return;
    }

    if let CrosstermEvent::Paste(text) = event {
        if app.modal == ActiveModal::None {
            app.insert_str(&text);
        }
        return;
    }

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
            ActiveModal::Variants => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.modal = ActiveModal::None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(ref mut vm) = app.variants_modal {
                            vm.previous();
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(ref mut vm) = app.variants_modal {
                            vm.next();
                        }
                    }
                    KeyCode::Enter => {
                        let selected = app
                            .variants_modal
                            .as_ref()
                            .and_then(|vm| vm.selected_variant().cloned());

                        if let Some(item) = selected {
                            app.modal = ActiveModal::None;
                            app.switch_variant(item.id.as_deref()).await;
                        }
                    }
                    _ => {}
                }
                return;
            }
            ActiveModal::Config => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.modal = ActiveModal::None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(ref mut cm) = app.config_modal {
                            cm.previous();
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(ref mut cm) = app.config_modal {
                            cm.next();
                        }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let selected_kind = app
                            .config_modal
                            .as_ref()
                            .and_then(|cm| cm.selected_item().map(|it| it.kind));

                        if let Some(kind) = selected_kind {
                            match kind {
                                app::ConfigItemKind::Model => {
                                    app.modal = ActiveModal::None;
                                    app.open_models_modal().await;
                                }
                                app::ConfigItemKind::Variant => {
                                    app.modal = ActiveModal::None;
                                    app.open_variants_modal().await;
                                }
                                app::ConfigItemKind::WebSearch => {
                                    app.enable_web_search = !app.enable_web_search;
                                    let status = if app.enable_web_search { "ON" } else { "OFF" };
                                    app.chat_entries.push(app::ChatEntry::SystemNotice {
                                        text: format!("Native web search is now {status}"),
                                    });
                                    if let Some(ref mut cm) = app.config_modal {
                                        cm.update_web_search(app.enable_web_search);
                                    }
                                }
                                app::ConfigItemKind::ThinkingDisplay => {
                                    app.thinking_concise = !app.thinking_concise;
                                    let mode_str = if app.thinking_concise {
                                        "concise (1 line)"
                                    } else {
                                        "full"
                                    };
                                    app.chat_entries.push(app::ChatEntry::SystemNotice {
                                        text: format!("Thinking display mode set to {mode_str}."),
                                    });
                                    if let Some(ref mut cm) = app.config_modal {
                                        cm.update_thinking_display(app.thinking_concise);
                                    }
                                }
                            }
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
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT)
                        || key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.matched_commands.clear();
                        app.selected_command_index = 0;
                        app.insert_char('\n');
                        return;
                    }
                    if let Some(cmd) = app.matched_commands.get(app.selected_command_index) {
                        app.set_input(cmd.name);
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
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    app.insert_char('\n');
                } else {
                    app.submit_input().await;
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.insert_char('\n');
            }
            KeyCode::Char('\n') => {
                app.insert_char('\n');
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
            KeyCode::Up => {
                if app.input.is_empty() {
                    app.scroll_up(2);
                } else {
                    app.move_cursor_up();
                }
            }
            KeyCode::Down => {
                if app.input.is_empty() {
                    app.scroll_down(2);
                } else {
                    app.move_cursor_down();
                }
            }
            KeyCode::Home => {
                if app.input.is_empty() {
                    app.scroll_to_top();
                } else {
                    app.move_cursor_home();
                }
            }
            KeyCode::End => {
                if app.input.is_empty() {
                    app.scroll_to_bottom();
                } else {
                    app.move_cursor_end();
                }
            }
            KeyCode::PageUp => {
                let step = app.viewport_height.saturating_sub(2).max(5);
                app.scroll_up(step);
            }
            KeyCode::PageDown => {
                let step = app.viewport_height.saturating_sub(2).max(5);
                app.scroll_down(step);
            }
            KeyCode::Esc if !app.input.is_empty() => {
                app.clear_input();
            }
            _ => {}
        }
    }
}
