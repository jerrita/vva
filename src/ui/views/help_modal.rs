use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::command::COMMANDS;

/// 渲染帮助与快捷键弹窗
pub fn render_help_modal(frame: &mut Frame, area: Rect) {
    let modal_width = 74.min(area.width.saturating_sub(4));
    let modal_height = 22.min(area.height.saturating_sub(4));

    let modal_rect = Rect {
        x: (area.width - modal_width) / 2,
        y: (area.height - modal_height) / 2,
        width: modal_width,
        height: modal_height,
    };

    frame.render_widget(Clear, modal_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Help & Keyboard Shortcuts ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "Available Slash Commands:",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    for cmd in COMMANDS {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<14}", cmd.name),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("- {}", cmd.description),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Keyboard Shortcuts:",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::styled("  Enter        ", Style::default().fg(Color::Green)),
        Span::styled(
            "- Submit prompt / execute selected command",
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Ctrl+Enter   ", Style::default().fg(Color::Green)),
        Span::styled(
            "- Insert newline in input (also Shift+Enter / Alt+Enter)",
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Tab          ", Style::default().fg(Color::Green)),
        Span::styled(
            "- Autocomplete command in input / toggle project scope in /sessions",
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Up / Down    ", Style::default().fg(Color::Green)),
        Span::styled(
            "- Move cursor between lines in input / navigate suggestions",
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  PgUp / PgDn  ", Style::default().fg(Color::Green)),
        Span::styled(
            "- Scroll conversation history up and down",
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Esc          ", Style::default().fg(Color::Green)),
        Span::styled(
            "- Close popup modal / clear input",
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Ctrl+C       ", Style::default().fg(Color::Green)),
        Span::styled(
            "- Abort current agent run / quit application",
            Style::default().fg(Color::White),
        ),
    ]));

    let p = Paragraph::new(lines);
    frame.render_widget(p, chunks[0]);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" Press ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[Esc]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" or ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[q]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " to close this dialog",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    frame.render_widget(footer, chunks[1]);
}
