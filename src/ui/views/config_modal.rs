use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::ui::app::{App, ConfigItemKind};
use crate::ui::views::modal_shell::{
    centered_rect, filter_line, footer_hints, modal_block, selected_row_style, visible_window,
};

/// 渲染 /config 模态弹窗，支持上下键或 j/k 浏览与调整各项会话配置（模型、推理档位、网络搜索等）
pub fn render_config_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.config_modal {
        Some(s) => s,
        None => return,
    };

    let modal_rect = centered_rect(area, 80, 16);
    frame.render_widget(Clear, modal_rect);

    let block = modal_block(
        " Configuration & Session Settings (↑/↓ navigate, Enter/Space to adjust) ",
        Color::Cyan,
    );

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 顶部提示与搜索条件
            Constraint::Min(1),    // 配置列表
            Constraint::Length(1), // 底部操作按键提示
        ])
        .split(inner);

    let top_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Manage active session settings interactively:",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(vec![top_line, filter_line(&state.query, Color::Cyan)]),
        chunks[0],
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

    let (start_idx, visible) = visible_window(
        state.selected_index,
        chunks[1].height.saturating_sub(1) as usize,
    );

    for (i, item) in state
        .filtered_items
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(visible)
    {
        let is_selected = i == state.selected_index;
        let prefix = if is_selected { "> " } else { "  " };

        let row_style = if is_selected {
            selected_row_style(Color::Cyan)
        } else {
            Style::default().fg(Color::White)
        };

        let val_style = if is_selected {
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
                if is_selected {
                    row_style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), chunks[1]);

    let footer = footer_hints(&[
        (" [↑/↓/j/k] ", Style::default().fg(Color::Cyan), "Navigate  "),
        (
            " [Enter/Space] ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            "Modify / Toggle  ",
        ),
        (" [type] ", Style::default().fg(Color::Cyan), "Filter  "),
        (" [Esc] ", Style::default().fg(Color::Yellow), "Close"),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}
