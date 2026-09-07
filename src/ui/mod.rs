pub mod app;
pub mod command;
pub mod history;
mod hyperlinks;
pub mod input;
pub mod markdown;
pub mod modals;
pub mod text;
pub mod views;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(not(windows))]
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyCode, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use std::io::{self, stdout};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::AppConfig;
use crate::harness::SessionManager;
use crate::storage::SessionStore;
use app::{ActiveModal, App};
use modals::{ModalAction, ModalInput, classify_modal_key};
use views::chat::render_chat;
use views::config_modal::render_config_modal;
use views::help_modal::render_help_modal;
use views::input_area::render_input_area;
use views::message_modal::{render_fork_title_modal, render_message_action_modal};
use views::modal_shell::modal_list_hit_index;
use views::models_modal::render_models_modal;
use views::sessions_modal::render_sessions_modal;
use views::sidebar::{SidebarInfo, render_sidebar};
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
    let mut events = spawn_crossterm_events();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
    let mut hyperlinks = hyperlinks::Hyperlinks::default();

    loop {
        let view = app.view.clone();
        let session = &view.session;
        let session_id = &session.id;
        let session_model = &session.current_model;
        let session_variant = &session.current_variant;
        let session_workspace = session.workspace_root.display().to_string();

        let frame = terminal.draw(|frame| {
            let size = frame.area();

            let input_lines = app.input.line_count();
            // 输入区：圆角输入框（边框 2 + 文本行）与元信息行
            let max_text_rows = ((size.height.saturating_sub(8)) / 2).clamp(1, 12) as usize;
            let text_rows = input_lines.clamp(1, max_text_rows);
            let input_area_height = (text_rows as u16) + 3;

            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(input_area_height)])
                .split(size);

            // 宽屏时 (>= 90 宽) 上半区切出右侧详情边栏，与主窗口齐平
            let (chat_area, sidebar_area) = if size.width >= 90 {
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

            render_input_area(
                app,
                frame,
                rows[1],
                session_model,
                session_variant.as_deref(),
                &session_workspace,
            );

            if let Some(sidebar_rect) = sidebar_area {
                let info = SidebarInfo {
                    session_id,
                    workspace: &session_workspace,
                    model: session_model,
                    variant: session_variant.as_deref(),
                };
                render_sidebar(app, frame, sidebar_rect, &info);
            } else {
                app.sidebar.reset();
            }

            // 弹窗视图回写列表区几何，供鼠标命中测试
            app.modal_list_area = match &app.modal {
                ActiveModal::Sessions(state) => render_sessions_modal(state, frame, size),
                ActiveModal::Models(state) => {
                    render_models_modal(state, session_model, frame, size)
                }
                ActiveModal::Variants(state) => render_variants_modal(state, frame, size),
                ActiveModal::Config(state) => render_config_modal(state, frame, size),
                ActiveModal::MessageAction(state) => {
                    render_message_action_modal(state, frame, size)
                }
                ActiveModal::ForkTitle(state) => {
                    render_fork_title_modal(state, frame, size);
                    Rect::default()
                }
                ActiveModal::Help => {
                    render_help_modal(frame, size);
                    Rect::default()
                }
                ActiveModal::None => Rect::default(),
            }
        })?;
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
                    handle_crossterm_event(app, crossterm_event, &mut events).await;
                    drain_ready_events(app, &mut events).await;
                    break;
                }
            }
        }
    }

    Ok(())
}

fn spawn_crossterm_events() -> mpsc::UnboundedReceiver<CrosstermEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("vva-crossterm-events".into())
        .spawn(move || {
            while let Ok(event) = crossterm::event::read() {
                if tx.send(event).is_err() {
                    break;
                }
            }
        })
        .expect("crossterm event thread");
    rx
}

/// 排空通道中已到达的事件，避免拖拽等高频输入逐次全量重绘
async fn drain_ready_events(app: &mut App, events: &mut mpsc::UnboundedReceiver<CrosstermEvent>) {
    while let Ok(event) = events.try_recv() {
        handle_crossterm_event(app, event, events).await;
        if app.should_quit {
            return;
        }
    }
}

async fn handle_crossterm_event(
    app: &mut App,
    event: CrosstermEvent,
    events: &mut mpsc::UnboundedReceiver<CrosstermEvent>,
) {
    if let CrosstermEvent::Mouse(mouse) = event {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if app.modal.is_none() {
                    app.scroll_up(3);
                } else {
                    app.modal.apply_input(ModalInput::NavigateUp);
                }
            }
            MouseEventKind::ScrollDown => {
                if app.modal.is_none() {
                    app.scroll_down(3);
                } else {
                    app.modal.apply_input(ModalInput::NavigateDown);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if app.modal.is_none() {
                    handle_mouse_left_down(app, mouse).await;
                } else {
                    handle_modal_click(app, mouse).await;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if app.modal.is_none() {
                    handle_mouse_left_drag(app, mouse);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let user_click = (app.modal.is_none()
                    && app.chat.selection.is_selecting()
                    && app.chat.selection.get().is_some_and(|sel| sel.is_empty()))
                .then(|| app.chat.selection.get().map(|sel| sel.anchor.line))
                .flatten();
                app.is_dragging_scrollbar = false;
                app.finish_selections();
                if let Some(line) = user_click
                    && !app.handle_tool_click(line)
                    && !app.handle_thought_click(line)
                {
                    app.open_message_action_at_line(line);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if app.copy_active_selection() {
                    return;
                }
                if app.modal.is_none() {
                    if app.chat.hit(mouse.column, mouse.row) {
                        let (row_offset, _) = app.chat.offsets(mouse.column, mouse.row);
                        if app.handle_tool_right_click(app.chat_scroll() + row_offset) {
                            return;
                        }
                    }
                    // 右键档位标签：循环切换推理档位；其余区域保持粘贴语义
                    if rect_hit(app.variant_selector_area, mouse.column, mouse.row) {
                        app.cycle_variant().await;
                        return;
                    }
                    app.paste_from_clipboard();
                }
            }
            _ => {}
        }
        return;
    }

    if let CrosstermEvent::Paste(text) = event {
        if app.modal.is_none() {
            app.insert_str(&text);
        }
        return;
    }

    if let CrosstermEvent::Key(key) = event {
        if key.kind != KeyEventKind::Press {
            return;
        }

        // 弹窗打开时独占按键
        if !app.modal.is_none() {
            handle_modal_key(app, key.code).await;
            return;
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
            // Ctrl+C 语义级联：复制选区 → 清空输入 → 中止当前轮 → 退出
            KeyCode::Char(_) if is_ctrl_key(&key, 'c') => {
                handle_abort_or_quit(app).await;
            }
            // Ctrl+A / Ctrl+E 光标首尾移动
            KeyCode::Char(_) if is_ctrl_key(&key, 'a') => {
                app.input.move_cursor_home();
            }
            KeyCode::Char(_) if is_ctrl_key(&key, 'e') => {
                app.input.move_cursor_end();
            }
            // Ctrl+K / Ctrl+U 清空输入框内容
            KeyCode::Char(_) if is_ctrl_key(&key, 'k') || is_ctrl_key(&key, 'u') => {
                app.clear_input();
            }
            // 剪贴板粘贴快捷键 (Ctrl+V / Ctrl+Shift+V / Shift+Insert / ASCII 0x16)
            KeyCode::Char(_) if is_ctrl_key(&key, 'v') => {
                if app.modal.is_none() {
                    app.paste_from_clipboard();
                }
            }
            KeyCode::Insert if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if app.modal.is_none() {
                    app.paste_from_clipboard();
                }
            }
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    app.insert_char('\n');
                } else {
                    let is_rapid_paste = app
                        .last_key_input_time
                        .map(|t| t.elapsed() < Duration::from_millis(40))
                        .unwrap_or(false);

                    if is_rapid_paste {
                        app.insert_char('\n');
                    } else {
                        let upcoming_event = tokio::select! {
                            next_ev = events.recv() => next_ev,
                            _ = tokio::time::sleep(Duration::from_millis(15)) => None,
                        };

                        if let Some(next_ev) = upcoming_event {
                            app.insert_char('\n');
                            Box::pin(handle_crossterm_event(app, next_ev, events)).await;
                        } else {
                            app.submit_input().await;
                        }
                    }
                }
            }
            // Ctrl+J / 部分终端将 Enter 发送为裸 '\n' 控制字符
            KeyCode::Char(_) if is_ctrl_key(&key, 'j') => {
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
                app.input.move_cursor_left();
            }
            KeyCode::Right => {
                app.input.move_cursor_right();
            }
            KeyCode::Up => {
                let (row, _) = app.input.cursor_line_and_col();
                if app.input.is_empty() || app.history_index.is_some() || row == 0 {
                    if !app.prompt_history.is_empty() {
                        app.history_previous();
                    } else if app.input.is_empty() {
                        app.scroll_up(2);
                    }
                } else {
                    app.input.move_cursor_up();
                }
            }
            KeyCode::Down => {
                let (row, _) = app.input.cursor_line_and_col();
                let line_count = app.input.line_count();
                if app.history_index.is_some() {
                    app.history_next();
                } else if app.input.is_empty() {
                    app.scroll_down(2);
                } else if row + 1 < line_count {
                    app.input.move_cursor_down();
                }
            }
            KeyCode::Home => {
                if app.input.is_empty() {
                    app.scroll_to_top();
                } else {
                    app.input.move_cursor_home();
                }
            }
            KeyCode::End => {
                if app.input.is_empty() {
                    app.scroll_to_bottom();
                } else {
                    app.input.move_cursor_end();
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
            KeyCode::Esc => {
                handle_escape(app).await;
            }
            _ => {}
        }
    }
}

/// 弹窗列表左键点击：命中条目则选中，再次点击已选中条目即确认
async fn handle_modal_click(app: &mut App, mouse: crossterm::event::MouseEvent) {
    if matches!(&app.modal, ActiveModal::Sessions(state) if state.pending_delete().is_some()) {
        return;
    }
    let (items_len, selected) = match app.modal.as_filterable_mut() {
        Some(modal) => (modal.filtered_len(), *modal.selected_index_mut()),
        None => return,
    };
    let Some(index) = modal_list_hit_index(
        app.modal_list_area,
        items_len,
        selected,
        mouse.column,
        mouse.row,
    ) else {
        return;
    };
    if index == selected {
        let action = app.modal.activate();
        apply_modal_action(app, action).await;
    } else if let Some(modal) = app.modal.as_filterable_mut() {
        *modal.selected_index_mut() = index;
    }
}

/// 弹窗按键：归一化 → 关闭 / 确认动作 / 交给状态机
async fn handle_modal_key(app: &mut App, code: KeyCode) {
    let input = match &mut app.modal {
        ActiveModal::Help => {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                app.modal = ActiveModal::None;
            }
            return;
        }
        ActiveModal::MessageAction(_) => match code {
            KeyCode::Esc => ModalInput::Close,
            KeyCode::Enter => ModalInput::Activate,
            KeyCode::Up | KeyCode::Char('k') => ModalInput::NavigateUp,
            KeyCode::Down | KeyCode::Char('j') => ModalInput::NavigateDown,
            _ => return,
        },
        ActiveModal::ForkTitle(_) if matches!(code, KeyCode::Esc | KeyCode::Enter) => {
            if matches!(code, KeyCode::Esc) {
                if let ActiveModal::ForkTitle(state) =
                    std::mem::replace(&mut app.modal, ActiveModal::None)
                {
                    app.modal = ActiveModal::MessageAction(state.into_message_action());
                }
            } else {
                let action = app.modal.activate();
                apply_modal_action(app, action).await;
            }
            return;
        }
        ActiveModal::ForkTitle(state) => {
            match code {
                KeyCode::Backspace => state.title.backspace(),
                KeyCode::Delete => state.title.delete(),
                KeyCode::Left => state.title.move_cursor_left(),
                KeyCode::Right => state.title.move_cursor_right(),
                KeyCode::Home => state.title.move_cursor_home(),
                KeyCode::End => state.title.move_cursor_end(),
                KeyCode::Char(c) if !c.is_control() => state.title.insert_char(c),
                _ => {}
            }
            return;
        }
        ActiveModal::Sessions(state) if state.pending_delete().is_some() => match code {
            KeyCode::Esc => {
                state.cancel_delete();
                return;
            }
            KeyCode::Enter | KeyCode::Char('d') => {
                let action = app.modal.delete_selected();
                apply_modal_action(app, action).await;
                return;
            }
            _ => return,
        },
        // 会话弹窗：Tab / p 切换工程范围
        ActiveModal::Sessions(state) if matches!(code, KeyCode::Tab | KeyCode::Char('p')) => {
            state.toggle_scope();
            return;
        }
        ActiveModal::Sessions(_) if matches!(code, KeyCode::Char('d')) => {
            if let ActiveModal::Sessions(state) = &mut app.modal {
                state.request_delete();
            }
            return;
        }
        // 设置弹窗：空格与 Enter 同为激活键
        ActiveModal::Config(_) if matches!(code, KeyCode::Char(' ')) => ModalInput::Activate,
        _ => classify_modal_key(code, &[]),
    };

    match input {
        ModalInput::Close => app.modal = ActiveModal::None,
        ModalInput::Activate => {
            let action = app.modal.activate();
            apply_modal_action(app, action).await;
        }
        input => app.modal.apply_input(input),
    }
}

/// 执行弹窗确认动作：切换类动作关闭弹窗，开关类动作原地生效
async fn apply_modal_action(app: &mut App, action: ModalAction) {
    match action {
        ModalAction::SwitchSession(id) => {
            app.modal = ActiveModal::None;
            if let Err(error) = app.switch_to_session(&id).await {
                app.notices.push(error);
            }
        }
        ModalAction::DeleteSession(id) => {
            if id == app.active_session.id {
                app.modal = ActiveModal::None;
                if let Err(error) = app.session_mgr.delete_session(&app.active_session).await {
                    app.notices.push(error);
                } else if let Err(error) = app.new_session().await {
                    app.notices.push(error);
                }
            } else {
                match app.session_mgr.get_or_load(&id).await {
                    Ok(session) => match app.session_mgr.delete_session(&session).await {
                        Ok(()) => {
                            if let ActiveModal::Sessions(state) = &mut app.modal {
                                state.remove_session(&id);
                            }
                        }
                        Err(error) => app.notices.push(error),
                    },
                    Err(error) => app.notices.push(error),
                }
            }
        }
        ModalAction::SwitchModel(spec) => {
            app.modal = ActiveModal::None;
            app.switch_model(&spec).await;
        }
        ModalAction::SwitchVariant(variant) => {
            app.modal = ActiveModal::None;
            app.switch_variant(variant.as_deref()).await;
        }
        ModalAction::OpenModels => {
            app.modal = ActiveModal::None;
            app.open_models_modal().await;
        }
        ModalAction::OpenVariants => {
            app.modal = ActiveModal::None;
            app.open_variants_modal().await;
        }
        ModalAction::ToggleWebSearch => app.set_web_search(None).await,
        ModalAction::ToggleThinkingDisplay => app.toggle_thinking_display(),
        ModalAction::RevertTurn { turn_index, prompt } => {
            app.revert_user_turn(turn_index, prompt).await;
        }
        ModalAction::OpenForkTitle { turn_index, prompt } => {
            app.open_fork_title(turn_index, prompt);
        }
        ModalAction::ConfirmFork { turn_index, title } => {
            app.fork_user_turn(turn_index, title).await;
        }
        ModalAction::None => {}
    }
}

/// 判定是否为指定字母的 Ctrl 组合键（兼容未携带 CONTROL 修饰符、
/// 直接发送原始控制字符 0x01-0x1a 的终端）
fn is_ctrl_key(key: &crossterm::event::KeyEvent, canonical: char) -> bool {
    match key.code {
        KeyCode::Char(c) => {
            (c == canonical && key.modifiers.contains(KeyModifiers::CONTROL))
                || c == (canonical as u8 & 0x1f) as char
        }
        _ => false,
    }
}

/// Esc 在生成时中止当前轮；空闲时级联清除界面选区与输入。
async fn handle_escape(app: &mut App) {
    if app.active_session.is_running() {
        if let Err(error) = app.active_session.abort_current().await {
            app.notices.push(error);
        }
    } else {
        escape_clear(app);
    }
}

/// Esc 级联：清除输入选区 → 清空输入 → 清除聊天选区 → 清除侧边栏选区
fn escape_clear(app: &mut App) {
    if app.input.selection.is_some() {
        app.clear_input_selection();
    } else if !app.input.is_empty() {
        app.clear_input();
    } else if app.chat.selection.is_some() {
        app.chat.selection.clear();
    } else if app.sidebar.selection.is_some() {
        app.sidebar.selection.clear();
    }
}

/// Ctrl+C 级联：复制活跃选区 → 清空输入 → 中止当前轮 → 退出应用
async fn handle_abort_or_quit(app: &mut App) {
    if app.copy_active_selection() {
        return;
    }
    if !app.input.is_empty() {
        app.clear_input();
    } else if app.active_session.is_running() {
        if let Err(error) = app.active_session.abort_current().await {
            app.notices.push(error);
        }
    } else {
        app.should_quit = true;
    }
}

/// 鼠标左键按下：按命中面板启动输入框 / 侧边栏 / 聊天流选区或滚动条拖拽
async fn handle_mouse_left_down(app: &mut App, mouse: crossterm::event::MouseEvent) {
    let (column, row) = (mouse.column, mouse.row);

    if rect_hit(app.model_selector_area, column, row) {
        app.open_models_modal().await;
        return;
    }
    if rect_hit(app.variant_selector_area, column, row) {
        app.open_variants_modal().await;
        return;
    }
    if rect_hit(app.web_toggle_area, column, row) {
        app.set_web_search(None).await;
        return;
    }
    if rect_hit(app.thinking_toggle_area, column, row) {
        app.toggle_thinking_display();
        return;
    }

    // 1. 输入框：定位光标并启动输入选区
    if rect_hit(app.input_inner_area, column, row) {
        let rel_row = (row - app.input_inner_area.y) as usize + app.input.scroll_y.get();
        let rel_col = (column - app.input_inner_area.x) as usize + app.input.scroll_x.get();
        let char_pos = app.input.set_cursor_from_visual_coord(rel_row, rel_col);
        app.start_input_selection(char_pos);
        app.chat.selection.clear();
        app.sidebar.selection.clear();
        app.is_dragging_scrollbar = false;
        return;
    }
    app.clear_input_selection();

    // 2. 侧边栏：启动文本选区
    if app.sidebar.hit(column, row) {
        let (row_offset, col_offset) = app.sidebar.offsets(column, row);
        app.sidebar.selection.start(row_offset, col_offset);
        app.chat.selection.clear();
        app.is_dragging_scrollbar = false;
        return;
    }

    // 3. 聊天流：滚动条列启动拖拽，正文区启动文本选区
    if rect_hit(app.scrollbar_area, column, row) && app.max_scroll > 0 {
        app.is_dragging_scrollbar = true;
        app.scroll_to_ratio(scrollbar_ratio(app, row));
        return;
    }
    if app.chat.hit(column, row) {
        let text_end = app.chat.area.x + app.chat.area.width.saturating_sub(1);
        if column < text_end {
            app.is_dragging_scrollbar = false;
            let (row_offset, col_offset) = app.chat.offsets(column, row);
            app.chat
                .selection
                .start(app.chat_scroll() + row_offset, col_offset);
            app.sidebar.selection.clear();
        }
        return;
    }

    app.is_dragging_scrollbar = false;
    app.chat.selection.clear();
    app.sidebar.selection.clear();
}

/// 终端坐标命中矩形判定
fn rect_hit(area: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    area.width > 0
        && area.height > 0
        && area.contains(ratatui::layout::Position { x: column, y: row })
}

/// 滚动条行位置转换为滚动比例
fn scrollbar_ratio(app: &App, row: u16) -> f64 {
    let track_height = app.scrollbar_area.height as usize;
    let row_offset = (row as usize).saturating_sub(app.scrollbar_area.y as usize);
    row_offset as f64 / track_height.saturating_sub(1).max(1) as f64
}

/// 鼠标左键拖拽：按当前拖拽目标更新滚动条、输入选区、聊天选区或侧边栏选区
fn handle_mouse_left_drag(app: &mut App, mouse: crossterm::event::MouseEvent) {
    let (column, row) = (mouse.column, mouse.row);

    if app.is_dragging_scrollbar && app.max_scroll > 0 && app.scrollbar_area.height > 0 {
        let clamped_row = row.clamp(
            app.scrollbar_area.y,
            app.scrollbar_area.y + app.scrollbar_area.height.saturating_sub(1),
        );
        app.scroll_to_ratio(scrollbar_ratio(app, clamped_row));
    } else if app.is_selecting_input && app.input_inner_area.height > 0 {
        let rel_row = if row < app.input_inner_area.y {
            app.input.scroll_y.get().saturating_sub(1)
        } else {
            (row - app.input_inner_area.y) as usize + app.input.scroll_y.get()
        };
        let rel_col =
            (column.saturating_sub(app.input_inner_area.x)) as usize + app.input.scroll_x.get();
        let char_pos = app.input.set_cursor_from_visual_coord(rel_row, rel_col);
        app.update_input_selection(char_pos);
    } else if app.chat.selection.is_selecting() && app.chat.area.height > 0 {
        // 拖拽越过聊天区边界时自动滚动
        if row < app.chat.area.y {
            app.scroll_up(1);
        } else if row >= app.chat.area.y + app.chat.area.height {
            app.scroll_down(1);
        }
        let text_width = app.chat.area.width.saturating_sub(1);
        let (row_offset, col_offset) = app.chat.offsets(column, row);
        let col_offset = col_offset.min(text_width as usize);
        app.chat
            .selection
            .update(app.chat_scroll() + row_offset, col_offset);
    } else if app.sidebar.selection.is_selecting() && app.sidebar.area.height > 0 {
        let (row_offset, col_offset) = app.sidebar.offsets(column, row);
        app.sidebar.selection.update(row_offset, col_offset);
    }
}
