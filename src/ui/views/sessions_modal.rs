use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::app::{App, ProjectScope};

/// 渲染 /sessions 模态弹窗，支持基于当前工程过滤与切换全量工程
pub fn render_sessions_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.sessions_modal {
        Some(s) => s,
        None => return,
    };

    // 居中计算弹出框尺寸
    let modal_width = 86.min(area.width.saturating_sub(4));
    let modal_height = 20.min(area.height.saturating_sub(4));

    let modal_rect = Rect {
        x: (area.width - modal_width) / 2,
        y: (area.height - modal_height) / 2,
        width: modal_width,
        height: modal_height,
    };

    // 清空背景
    frame.render_widget(Clear, modal_rect);

    let title_span = match state.scope {
        ProjectScope::CurrentProject => Span::styled(
            " Select Session to Resume (Per Project) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        ProjectScope::AllProjects => Span::styled(
            " Select Session to Resume (All Projects) ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title_span);

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Scope 标签切换提示
            Constraint::Min(1),    // Session 列表
            Constraint::Length(1), // 底部操作按键提示
        ])
        .split(inner);

    // 1. 作用域切换标签
    let curr_count = state
        .all_sessions
        .iter()
        .filter(|s| {
            let norm_s = std::fs::canonicalize(&s.workspace_root)
                .unwrap_or_else(|_| s.workspace_root.clone());
            let norm_cur = std::fs::canonicalize(app.session_mgr.workspace())
                .unwrap_or_else(|_| app.session_mgr.workspace().to_path_buf());
            norm_s == norm_cur
        })
        .count();
    let all_count = state.all_sessions.len();

    let (curr_style, all_style) = match state.scope {
        ProjectScope::CurrentProject => (
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::DarkGray),
        ),
        ProjectScope::AllProjects => (
            Style::default().fg(Color::DarkGray),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let tabs_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!(" [ This Project: {} ] ", curr_count), curr_style),
        Span::raw("  "),
        Span::styled(format!(" [ All Projects: {} ] ", all_count), all_style),
        Span::raw("   "),
        Span::styled(
            "(Press Tab to switch scope)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);
    let tabs_p = Paragraph::new(tabs_line);
    frame.render_widget(tabs_p, chunks[0]);

    // 2. 会话列表渲染
    if state.filtered_sessions.is_empty() {
        let empty_msg = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    "No sessions found in this scope.",
                    Style::default().fg(Color::Yellow),
                ),
            ]),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    "Press [Tab] to toggle to All Projects or start chatting to create a new session.",
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(empty_msg), chunks[1]);
    } else {
        let mut lines = Vec::new();
        // 表头
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<16}", "Session ID"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<26}", "Title / Prompt"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<6}", "Turns"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<18}", "Model"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<10}", "Created"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        let list_visible_height = chunks[1].height.saturating_sub(1) as usize;
        let start_idx = if state.selected_index >= list_visible_height {
            state.selected_index - list_visible_height + 1
        } else {
            0
        };

        for (i, item) in state
            .filtered_sessions
            .iter()
            .enumerate()
            .skip(start_idx)
            .take(list_visible_height)
        {
            let is_selected = i == state.selected_index;
            let prefix = if is_selected { "> " } else { "  " };

            let short_id = if item.id.len() > 14 {
                format!("{}..", &item.id[..12])
            } else {
                item.id.clone()
            };

            let title_raw = item.title.as_deref().unwrap_or("Untitled");
            let short_title = if title_raw.chars().count() > 24 {
                format!("{}...", title_raw.chars().take(22).collect::<String>())
            } else {
                title_raw.to_string()
            };

            let short_model = if item.current_model.len() > 16 {
                format!("{}..", &item.current_model[..14])
            } else if item.current_model.is_empty() {
                "-".to_string()
            } else {
                item.current_model.clone()
            };

            let created_str = item.created_at.format("%m-%d %H:%M").to_string();

            let row_style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let turn_color = if is_selected {
                Color::Black
            } else {
                Color::Yellow
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<16}", short_id), row_style),
                Span::styled(format!("{:<26}", short_title), row_style),
                Span::styled(
                    format!("{:<6}", item.turn_count),
                    Style::default().fg(turn_color),
                ),
                Span::styled(format!("{:<18}", short_model), row_style),
                Span::styled(format!("{:<10}", created_str), row_style),
            ]));
        }

        let list_p = Paragraph::new(lines);
        frame.render_widget(list_p, chunks[1]);
    }

    // 3. 底部操作指引
    let footer_line = Line::from(vec![
        Span::styled(" [↑/↓/j/k] ", Style::default().fg(Color::Cyan)),
        Span::styled("Navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [Tab] ", Style::default().fg(Color::Cyan)),
        Span::styled("Toggle Scope  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " [Enter] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Resume Session  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [Esc/q] ", Style::default().fg(Color::Yellow)),
        Span::styled("Cancel", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer_line), chunks[2]);
}
