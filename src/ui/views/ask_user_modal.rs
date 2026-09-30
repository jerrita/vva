use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Color;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::ui::modals::AskUserModalState;
use crate::ui::views::modal_shell::{centered_rect, selected_row_style};
use crate::ui::views::{bold, fg};

pub fn render_ask_user_modal(state: &AskUserModalState, frame: &mut Frame, area: Rect) {
    let question = state.current_question();
    let height = (8 + question.options.len() as u16 * 2).min(area.height.saturating_sub(4));
    let rect = centered_rect(area, 76, height.max(8));
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(Color::Cyan))
        .title(Span::styled(" AskUser ", bold(Color::Cyan)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(format!(
            "  {}/{}{}",
            state.question_index + 1,
            state.request.questions.len(),
            if question.header.is_empty() {
                String::new()
            } else {
                format!(" · {}", question.header)
            }
        ))),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Text::from(question.question.clone())).wrap(Wrap { trim: true }),
        chunks[1],
    );
    let lines = question
        .options
        .iter()
        .enumerate()
        .flat_map(|(index, option)| {
            let selected = state.selected_option(index);
            let style = if index == state.option_index {
                selected_row_style(Color::Cyan)
            } else {
                fg(Color::White)
            };
            [
                Line::from(vec![
                    Span::styled(if selected { "  [x] " } else { "  [ ] " }, style),
                    Span::styled(option.label.clone(), style),
                ]),
                Line::from(Span::styled(
                    format!("      {}", option.description),
                    fg(Color::DarkGray),
                )),
            ]
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), chunks[2]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" [↑/↓] ", fg(Color::Cyan)),
            Span::styled("选择  ", fg(Color::DarkGray)),
            Span::styled(
                if question.multi_select {
                    " [Space] "
                } else {
                    " [Enter] "
                },
                fg(Color::Green),
            ),
            Span::styled(
                if question.multi_select {
                    "多选  "
                } else {
                    "确认  "
                },
                fg(Color::DarkGray),
            ),
            Span::styled(" [Esc] ", fg(Color::Yellow)),
            Span::styled("取消", fg(Color::DarkGray)),
        ])),
        chunks[3],
    );
}
