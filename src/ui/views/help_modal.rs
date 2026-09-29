use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::command::{COMMANDS, command_row};
use crate::ui::views::modal_shell::{ModalShellSpec, render_modal_shell};

/// 渲染帮助与快捷键弹窗
pub fn render_help_modal(frame: &mut Frame, area: Rect) {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "Available Slash Commands:",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    for cmd in COMMANDS {
        lines.push(command_row(
            cmd.name,
            cmd.description,
            "  ",
            14,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::White),
        ));
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

    let footer = Line::from(vec![
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
    ]);

    let content_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Help & Keyboard Shortcuts ",
            accent: Color::Cyan,
            width: 74,
            height: 22,
            header_height: 0,
        },
        Vec::new(),
        footer,
    );
    frame.render_widget(Paragraph::new(lines), content_area);
}
