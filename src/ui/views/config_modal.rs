use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::modals::{ConfigItemKind, ConfigModalState};
use crate::ui::views::modal_shell::{
    ModalShellSpec, filter_line, footer_hints, list_row_affix, render_modal_shell,
    selected_row_style, visible_window,
};

/// 渲染 /config 模态弹窗，支持上下键或 j/k 浏览与调整各项会话配置（模型、推理档位、网络搜索等）
pub fn render_config_modal(state: &ConfigModalState, frame: &mut Frame, area: Rect) {
    let top_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Manage active session settings interactively:",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);

    let list_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Configuration & Session Settings (↑/↓ navigate, Enter/Space to adjust) ",
            accent: Color::Cyan,
            width: 80,
            height: 16,
            header_height: 3,
        },
        vec![top_line, filter_line(&state.query, Color::Cyan)],
        footer_hints(&[
            (
                " [↑/↓/j/k] ",
                Style::default().fg(Color::Cyan),
                "Navigate  ",
            ),
            (
                " [Enter/Space] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "Modify / Toggle  ",
            ),
            (" [type] ", Style::default().fg(Color::Cyan), "Filter  "),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Close"),
        ]),
    );

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{:<24}", "Setting"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<20}", "Current Value"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("Action", Style::default().fg(Color::DarkGray)),
    ]));

    let (start_idx, visible) = visible_window(state.selected_index, list_area.height as usize);

    for (i, item) in state
        .filtered_items
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(visible)
    {
        let (prefix, row_style) = list_row_affix(i == state.selected_index, false, Color::Cyan);

        let val_style = if i == state.selected_index {
            selected_row_style(Color::Cyan)
        } else {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        };

        let action_str = match item.kind {
            ConfigItemKind::Model => "Enter to select model",
            ConfigItemKind::Variant => "Enter to select variant",
            ConfigItemKind::WebSearch => "Enter/Space to toggle",
            ConfigItemKind::ThinkingDisplay => "Enter/Space to toggle",
        };

        lines.push(Line::from(vec![
            Span::styled(prefix, row_style),
            Span::styled(format!("{:<24}", item.name), row_style),
            Span::styled(format!("{:<20}", item.current_value), val_style),
            Span::styled(
                action_str,
                if i == state.selected_index {
                    row_style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}
