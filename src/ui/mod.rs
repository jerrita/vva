pub mod app;
pub mod command;
pub(crate) mod events;
pub mod history;
mod hyperlinks;
pub mod input;
pub mod markdown;
pub mod modals;
mod spans;
pub mod text;
pub mod views;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(not(windows))]
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::{Frame, Terminal};
use std::io::{self, stdout};
use std::sync::Arc;
use std::time::Duration;

use crate::config::AppConfig;
use crate::harness::{SessionManager, SessionView};
use crate::storage::SessionStore;
use app::{ActiveModal, App};
use views::ask_user_modal::render_ask_user_modal;
use views::chat::render_chat;
use views::config_modal::render_config_modal;
use views::help_modal::render_help_modal;
use views::input_area::{feedback_lines, render_input_area};
use views::message_modal::{render_fork_title_modal, render_message_action_modal};
use views::models_modal::render_models_modal;
use views::sessions_modal::render_sessions_modal;
use views::sidebar::{SidebarInfo, render_sidebar};
use views::skills_modal::render_skills_modal;
use views::variants_modal::render_variants_modal;

/// 启动 Ratatui 交互式终端 UI
pub async fn run_tui(
    store: Arc<dyn SessionStore>,
    app_config: AppConfig,
    initial_session_id: Option<String>,
    workspace: std::path::PathBuf,
    default_model: Option<String>,
    default_variant: Option<String>,
    enable_web_search: Option<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_mgr = Arc::new(SessionManager::new(
        store,
        app_config,
        workspace,
        crate::config::runtime_state_path(),
    ));

    let initial_session = session_mgr
        .open_session(
            initial_session_id.as_deref(),
            default_model,
            default_variant,
            enable_web_search,
        )
        .await?;
    let mut app = App::new(session_mgr, initial_session).await;

    enable_raw_mode()?;
    let mut stdout = stdout();
    #[cfg(not(windows))]
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    #[cfg(windows)]
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let keyboard_enhancement = supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhancement {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app_loop(&mut terminal, &mut app).await;

    // 恢复终端
    if keyboard_enhancement {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    disable_raw_mode()?;
    #[cfg(not(windows))]
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    #[cfg(windows)]
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
    let mut events = events::spawn_crossterm_events();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
    let mut hyperlinks = hyperlinks::Hyperlinks::default();

    loop {
        let view = app.view.clone();
        let frame = terminal.draw(|frame| draw(app, frame, &view))?;
        let updates = hyperlinks.prepare(
            frame.buffer,
            &app.chat,
            app.chat_scroll(),
            app.modal.is_none(),
        );
        hyperlinks::write_updates(terminal.backend_mut(), &updates)?;

        if app.should_quit {
            break;
        }

        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    if app.poll_file_index() {
                        break;
                    }
                    if app.active_session.is_running() {
                        app.tick();
                        break;
                    }
                }

                changed = app.view_rx.changed() => {
                    if changed.is_err() { return Ok(()); }
                    app.refresh_view();
                    break;
                }

                maybe_event = events.recv() => {
                    let Some(crossterm_event) = maybe_event else { return Ok(()); };
                    events::handle_crossterm_event(app, crossterm_event, &mut events).await;
                    events::drain_ready_events(app, &mut events).await;
                    break;
                }
            }
        }
    }

    Ok(())
}

/// 绘制一帧：聊天区 / 反馈行 / 输入区 / 侧边栏，最后叠加当前弹窗
fn draw(app: &mut App, frame: &mut Frame, view: &SessionView) {
    let header = &view.session.header;
    let model = &header.current_model;
    let variant = header.current_variant.as_deref();
    let workspace = header.workspace_root.display().to_string();
    let size = frame.area();

    // 输入区：圆角输入框（边框 2 + 文本行）与元信息行
    let max_text_rows = (size.height.saturating_sub(8) / 2).clamp(1, 12) as usize;
    let input_area_height = app.input.line_count().clamp(1, max_text_rows) as u16 + 3;

    let feedback = feedback_lines(app, size.width as usize);
    let feedback_height =
        (feedback.len() as u16).min(size.height.saturating_sub(input_area_height + 5));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(feedback_height),
            Constraint::Length(input_area_height),
        ])
        .split(size);

    // 宽屏时 (>= 90 宽) 上半区切出右侧详情边栏，与主窗口齐平
    let (chat_area, sidebar_area) = if size.width >= 90 && app.show_sidebar {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(60), Constraint::Length(30)])
            .spacing(1)
            .split(rows[0]);
        (cols[0], Some(cols[1]))
    } else {
        (rows[0], None)
    };

    render_chat(app, frame, chat_area);

    let feedback_skip = feedback.len().saturating_sub(feedback_height as usize);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(feedback).scroll((feedback_skip as u16, 0)),
        rows[1],
    );
    render_input_area(app, frame, rows[2], model, variant, &workspace);

    if let Some(sidebar_rect) = sidebar_area {
        let info = SidebarInfo {
            session_id: &header.id,
            workspace: &workspace,
            model,
            variant,
        };
        render_sidebar(app, frame, sidebar_rect, &info);
    } else {
        app.sidebar.reset();
    }

    // 弹窗视图回写列表区几何，供鼠标命中测试
    app.modal_list_area = match &app.modal {
        ActiveModal::Sessions(state) => render_sessions_modal(state, frame, size),
        ActiveModal::Models(state) => render_models_modal(state, model, frame, size),
        ActiveModal::Variants(state) => render_variants_modal(state, frame, size),
        ActiveModal::Config(state) => render_config_modal(state, frame, size),
        ActiveModal::Skills(state) => render_skills_modal(state, frame, size),
        ActiveModal::MessageAction(state) => render_message_action_modal(state, frame, size),
        ActiveModal::ForkTitle(state) => {
            render_fork_title_modal(state, frame, size);
            Rect::default()
        }
        ActiveModal::AskUser(state) => {
            render_ask_user_modal(state, frame, size);
            Rect::default()
        }
        ActiveModal::Help => {
            render_help_modal(frame, size);
            Rect::default()
        }
        ActiveModal::None => Rect::default(),
    };
}
