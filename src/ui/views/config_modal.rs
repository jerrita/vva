use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::app::App;

/// 渲染 /config 模态弹窗，支持上下键或 j/k 浏览与调整各项会话配置（模型、推理档位、网络搜索等）
pub fn render_config_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.config_modal {
        Some(s) => s,
        None => return,
    };

    let modal_width = 80.min(area.width.saturating_sub(4));
    let modal_height = 16.min(area.height.saturating_sub(4));

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
            " Configuration & Session Settings (↑/↓ navigate, Enter/Space to adjust) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // 顶部提示
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
    frame.render_widget(Paragraph::new(top_line), chunks[0]);

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

    for (i, item) in state.items.iter().enumerate() {
        let is_selected = i == state.selected_index;
        let prefix = if is_selected { "> " } else { "  " };

        let row_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let val_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        };

        let action_str = match item.kind {
            crate::ui::app::ConfigItemKind::Model => "Enter to select model",
            crate::ui::app::ConfigItemKind::Variant => "Enter to select variant",
            crate::ui::app::ConfigItemKind::WebSearch => "Enter/Space to toggle",
            crate::ui::app::ConfigItemKind::ThinkingDisplay => "Enter/Space to toggle",
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

    let list_p = Paragraph::new(lines);
    frame.render_widget(list_p, chunks[1]);

    let footer_line = Line::from(vec![
        Span::styled(" [↑/↓/j/k] ", Style::default().fg(Color::Cyan)),
        Span::styled("Navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " [Enter/Space] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Modify / Toggle  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [Esc/q] ", Style::default().fg(Color::Yellow)),
        Span::styled("Close", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer_line), chunks[2]);
}
