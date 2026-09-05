use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::app::{App, ProjectScope};
use crate::ui::text::truncate;
use crate::ui::views::modal_shell::{
    ModalShellSpec, empty_state, filter_line, footer_hints, list_row_affix, render_modal_shell,
    selected_row_style, visible_window,
};

/// 渲染 /sessions 模态弹窗，支持基于当前工程过滤与切换全量工程
pub fn render_sessions_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.sessions_modal {
        Some(s) => s,
        None => return,
    };

    let title = match state.scope {
        ProjectScope::CurrentProject => " Select Session to Resume (Per Project) ",
        ProjectScope::AllProjects => " Select Session to Resume (All Projects) ",
    };

    // 作用域切换标签（计数由状态机在过滤时缓存，渲染不做文件系统调用）
    let curr_count = state.current_project_count;
    let all_count = state.all_sessions.len();

    let (curr_style, all_style) = match state.scope {
        ProjectScope::CurrentProject => (
            selected_row_style(Color::Cyan),
            Style::default().fg(Color::DarkGray),
        ),
        ProjectScope::AllProjects => (
            Style::default().fg(Color::DarkGray),
            selected_row_style(Color::Magenta),
        ),
    };

    let tabs_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!(" [ This Project: {curr_count} ] "), curr_style),
        Span::raw("  "),
        Span::styled(format!(" [ All Projects: {all_count} ] "), all_style),
        Span::raw("   "),
        Span::styled(
            "(Press Tab to switch scope)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);

    let list_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title,
            accent: Color::Cyan,
            width: 86,
            height: 20,
            header_height: 3,
        },
        vec![tabs_line, filter_line(&state.query, Color::Cyan)],
        footer_hints(&[
            (
                " [↑/↓/j/k] ",
                Style::default().fg(Color::Cyan),
                "Navigate  ",
            ),
            (
                " [Tab] ",
                Style::default().fg(Color::Cyan),
                "Toggle Scope  ",
            ),
            (
                " [Enter] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "Resume Session  ",
            ),
            (" [type] ", Style::default().fg(Color::Cyan), "Filter  "),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Cancel"),
        ]),
    );

    // 会话列表渲染
    if state.filtered_sessions.is_empty() {
        frame.render_widget(
            Paragraph::new(empty_state(
                "No sessions match this scope and filter.",
                Some("Press [Tab] to toggle to All Projects or start chatting to create a new session."),
            )),
            list_area,
        );
        return;
    }

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

    let (start_idx, visible) = visible_window(state.selected_index, list_area.height as usize);

    for (i, item) in state
        .filtered_sessions
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(visible)
    {
        let is_selected = i == state.selected_index;
        let (prefix, row_style) = list_row_affix(is_selected, false, Color::Cyan);

        let short_id = truncate(&item.id, 14);
        let title_raw = item.title.as_deref().unwrap_or("Untitled");
        let short_title = truncate(title_raw, 24);
        let short_model = if item.current_model.is_empty() {
            "-".to_string()
        } else {
            truncate(&item.current_model, 16)
        };
        let created_str = item.created_at.format("%m-%d %H:%M").to_string();

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

    frame.render_widget(Paragraph::new(lines), list_area);
}
