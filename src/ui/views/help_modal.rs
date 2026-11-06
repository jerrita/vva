use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::ui::command::{COMMANDS, command_row};
use crate::ui::views::modal_shell::{ModalShellSpec, render_modal_shell};
use crate::ui::views::{bold, fg};

/// 渲染帮助与快捷键弹窗
pub fn render_help_modal(frame: &mut Frame, area: Rect, scroll: &mut usize) {
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
    for (key, description) in [
        (
            "Ctrl+O",
            "Enter / leave chat browsing (starts at latest block)",
        ),
        ("Ctrl+T", "Cycle reasoning variant, including default"),
        (
            "Ctrl+V / Ctrl+Shift+V / Shift+Insert",
            "Paste clipboard into input; terminal paste also works",
        ),
        (
            "Chat browsing:",
            "Draft stays intact; Esc returns to editing",
        ),
        (
            "Up/Down · k/j · Shift+Tab/Tab",
            "Previous / next message or block",
        ),
        ("Home / End", "First / last block"),
        (
            "Enter / Space / Right / l",
            "Expand block; user message opens Revert / Fork",
        ),
        ("Left / h", "Fold block; tool full → preview → hidden"),
        ("y / Shift+Y", "Copy displayed block / entire sidebar"),
        ("PgUp / PgDn", "Scroll conversation (also while editing)"),
        ("Input:", ""),
        (
            "Enter",
            "Send / queue prompt / promote pending message to steer",
        ),
        (
            "Ctrl+Enter / Shift+Enter / Alt+Enter / Ctrl+J",
            "Insert newline",
        ),
        ("Tab", "Autocomplete; toggle project scope in /sessions"),
        (
            "Up / Down",
            "Move cursor / history / pending messages / suggestions",
        ),
        ("Ctrl+A / Ctrl+E", "Move input cursor to start / end"),
        ("Ctrl+K / Ctrl+U", "Clear input"),
        ("Esc", "Close popup / steer → after → edit pending message"),
        ("Ctrl+C", "Copy selection → clear input → abort run → quit"),
    ] {
        lines.push(Line::from(Span::styled(
            format!("  {key}"),
            fg(Color::Green),
        )));
        if !description.is_empty() {
            lines.push(Line::from(format!("    {description}")));
        }
    }

    let footer = Line::from(" ↑↓/PgUp/PgDn scroll · Home/End · Esc/q close ");

    let content_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Help & Keyboard Shortcuts ",
            accent: Color::Cyan,
            width: 88,
            height: 38,
            header_height: 0,
        },
        Vec::new(),
        footer,
    );
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    *scroll = (*scroll).min(
        paragraph
            .line_count(content_area.width)
            .saturating_sub(content_area.height as usize),
    );
    frame.render_widget(paragraph.scroll((*scroll as u16, 0)), content_area);
}
