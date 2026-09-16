use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::ui::modals::{ProjectScope, SessionsModalState};
use crate::ui::text::{pad_to_width, truncate};
use crate::ui::views::modal_shell::{
    ModalShellSpec, filter_line, footer_hints, list_row_affix, render_modal_list,
    render_modal_shell, selected_row_style,
};

/// 渲染 /sessions 模态弹窗，支持基于当前工程过滤与切换全量工程；
/// 返回列表区几何供鼠标命中测试
pub fn render_sessions_modal(state: &SessionsModalState, frame: &mut Frame, area: Rect) -> Rect {
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

    let confirming_delete = state.pending_delete();
    let mut header_lines = vec![tabs_line];
    let footer = if let Some(session_id) = confirming_delete {
        header_lines.push(Line::from(vec![
            Span::styled("  Delete ", Style::default().fg(Color::Red)),
            Span::styled(
                truncate(session_id, 24),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled("? This cannot be undone.", Style::default().fg(Color::Red)),
        ]));
        footer_hints(&[
            (
                " [Enter/d] ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                "Confirm Delete  ",
            ),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Cancel"),
        ])
    } else {
        header_lines.push(filter_line(&state.query, Color::Cyan));
        footer_hints(&[
            (" [↑↓] ", Style::default().fg(Color::Cyan), "Nav  "),
            (" [Tab] ", Style::default().fg(Color::Cyan), "Scope  "),
            (
                " [Enter] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "Resume  ",
            ),
            (
                " [d] ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                "Delete  ",
            ),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Cancel"),
        ])
    };

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
        header_lines,
        footer,
    );

    let id_width = 16;
    let turns_width = 6;
    let created_width = 11;
    let fixed_width = 2 + id_width + turns_width + created_width;
    let available_title_width = (list_area.width as usize).saturating_sub(fixed_width);
    let title_width = state
        .filtered_sessions
        .iter()
        .map(|item| item.title.as_deref().unwrap_or("Untitled").width())
        .max()
        .unwrap_or(0)
        .max("Title / Prompt".width())
        .min(available_title_width);
    let remaining_width = available_title_width.saturating_sub(title_width);
    let right_padding = remaining_width.min(2);
    let distributable_width = remaining_width - right_padding;
    let column_gap = distributable_width / 3;
    let final_gap = column_gap + distributable_width % 3;
    let gap = " ".repeat(column_gap);
    let final_gap = " ".repeat(final_gap);
    let right_padding = " ".repeat(right_padding);

    // 表头
    let header = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            pad_to_width("Session ID", id_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(gap.clone()),
        Span::styled(
            pad_to_width("Title / Prompt", title_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(gap.clone()),
        Span::styled(
            pad_to_width("Turns", turns_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(final_gap.clone()),
        Span::styled(
            pad_to_width("Created", created_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(right_padding.clone()),
    ]);

    render_modal_list(
        frame,
        list_area,
        &state.filtered_sessions,
        state.selected_index,
        header,
        Some((
            "No sessions match this scope and filter.",
            Some(
                "Press [Tab] to toggle to All Projects or start chatting to create a new session.",
            ),
        )),
        |i, item| {
            let is_selected = i == state.selected_index;
            let (prefix, row_style) = list_row_affix(is_selected, false, Color::Cyan);

            let short_id = truncate(&item.id, id_width - 2);
            let title_raw = item.title.as_deref().unwrap_or("Untitled");
            let short_title = truncate(title_raw, title_width);
            let created_str = item.created_at.format("%m-%d %H:%M").to_string();
            let turns_style = if is_selected {
                row_style
            } else {
                Style::default().fg(Color::Yellow)
            };

            Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(pad_to_width(&short_id, id_width), row_style),
                Span::styled(gap.clone(), row_style),
                Span::styled(pad_to_width(&short_title, title_width), row_style),
                Span::styled(gap.clone(), row_style),
                Span::styled(
                    pad_to_width(&item.turn_count.to_string(), turns_width),
                    turns_style,
                ),
                Span::styled(final_gap.clone(), row_style),
                Span::styled(pad_to_width(&created_str, created_width), row_style),
                Span::styled(right_padding.clone(), row_style),
            ])
        },
    );

    list_area
}
