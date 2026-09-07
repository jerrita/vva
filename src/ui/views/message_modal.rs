use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::ui::modals::{ForkTitleState, MESSAGE_ACTIONS, MessageActionState};
use crate::ui::views::modal_shell::{
    ModalShellSpec, centered_rect, footer_hints, list_row_affix, modal_block, render_modal_list,
    render_modal_shell,
};

/// 渲染用户消息 Revert / Fork 选择弹窗，返回列表区供鼠标命中
pub fn render_message_action_modal(
    state: &MessageActionState,
    frame: &mut Frame,
    area: Rect,
) -> Rect {
    let preview = crate::ui::text::truncate(&state.prompt.replace('\n', " "), 48);
    let list_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Message ",
            accent: Color::Cyan,
            width: 64,
            height: 11,
            header_height: 2,
        },
        vec![Line::from(vec![
            Span::styled("  Prompt: ", Style::default().fg(Color::DarkGray)),
            Span::styled(preview, Style::default().fg(Color::White)),
        ])],
        footer_hints(&[
            (
                " [↑/↓/j/k] ",
                Style::default().fg(Color::Cyan),
                "Navigate  ",
            ),
            (
                " [Enter] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "Select  ",
            ),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Cancel"),
        ]),
    );

    render_modal_list(
        frame,
        list_area,
        &MESSAGE_ACTIONS,
        state.selected_index,
        Line::from(""),
        None,
        |i, item| {
            let (prefix, row_style) = list_row_affix(i == state.selected_index, false, Color::Cyan);
            Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<8}", item.0), row_style),
                Span::styled(item.1.to_string(), row_style),
            ])
        },
    );
    list_area
}

/// 渲染 Fork 标题编辑弹窗
pub fn render_fork_title_modal(state: &ForkTitleState, frame: &mut Frame, area: Rect) {
    let modal_rect = centered_rect(area, 60, 8);
    frame.render_widget(Clear, modal_rect);
    let block = modal_block(" Fork Session ", Color::Cyan);
    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  New session title:",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                state.title.text.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("_", Style::default().fg(Color::Cyan)),
        ])),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" [Enter] ", Style::default().fg(Color::Green)),
            Span::styled("Fork  ", Style::default().fg(Color::DarkGray)),
            Span::styled(" [Esc] ", Style::default().fg(Color::Yellow)),
            Span::styled("Back", Style::default().fg(Color::DarkGray)),
        ])),
        chunks[3],
    );
}
