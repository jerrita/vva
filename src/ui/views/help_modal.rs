use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::command::{COMMANDS, command_row};
use crate::ui::views::modal_shell::{ModalShellSpec, render_modal_shell};
use crate::ui::views::{bold, fg};

/// 渲染帮助与快捷键弹窗
pub fn render_help_modal(frame: &mut Frame, area: Rect) {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "Available Slash Commands:",
        bold(Color::Yellow),
    )));

    for cmd in COMMANDS {
        lines.push(command_row(
            cmd.name,
            cmd.description,
            "  ",
            14,
            bold(Color::Cyan),
            fg(Color::White),
        ));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Keyboard Shortcuts:",
        bold(Color::Yellow),
    )));
    lines.push(Line::from(vec![
        Span::styled("  Enter        ", fg(Color::Green)),
        Span::styled(
            "- Submit prompt / execute selected command",
            fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Ctrl+Enter   ", fg(Color::Green)),
        Span::styled(
            "- Insert newline in input (also Shift+Enter / Alt+Enter)",
            fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Tab          ", fg(Color::Green)),
        Span::styled(
            "- Autocomplete command in input / toggle project scope in /sessions",
            fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Up / Down    ", fg(Color::Green)),
        Span::styled(
            "- Move cursor between lines in input / navigate suggestions",
            fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  PgUp / PgDn  ", fg(Color::Green)),
        Span::styled(
            "- Scroll conversation history up and down",
            fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Esc          ", fg(Color::Green)),
        Span::styled("- Close popup modal / clear input", fg(Color::White)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Ctrl+C       ", fg(Color::Green)),
        Span::styled(
            "- Abort current agent run / quit application",
            fg(Color::White),
        ),
    ]));

    let footer = Line::from(vec![
        Span::styled(" Press ", fg(Color::DarkGray)),
        Span::styled("[Esc]", bold(Color::Yellow)),
        Span::styled(" or ", fg(Color::DarkGray)),
        Span::styled("[q]", bold(Color::Yellow)),
        Span::styled(" to close this dialog", fg(Color::DarkGray)),
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
