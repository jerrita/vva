use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::ui::app::App;
use crate::ui::views::modal_shell::{
    centered_rect, filter_line, footer_hints, modal_block, selected_row_style, visible_window,
};

/// 渲染 /variant 模态弹窗，支持上下键或 j/k 浏览选择当前模型可用的推理强度档位
pub fn render_variants_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.variants_modal {
        Some(s) => s,
        None => return,
    };

    let modal_rect = centered_rect(area, 60, 16);
    frame.render_widget(Clear, modal_rect);

    let block = modal_block(
        " Select Reasoning Variant (↑/↓ to navigate, Enter to switch) ",
        Color::Magenta,
    );

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 当前模型、当前生效 Variant 与搜索条件
            Constraint::Min(1),    // Variant 列表
            Constraint::Length(1), // 底部操作按键提示
        ])
        .split(inner);

    // 1. 顶部状态指示行
    let curr_model = &state.current_model;
    let curr_var = state.active_variant.as_deref().unwrap_or("none");
    let top_line = Line::from(vec![
        Span::raw("  "),
        Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            curr_model,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("    "),
        Span::styled("Active Variant: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            curr_var,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "({}/{} options)",
                state.filtered_variants.len(),
                state.variants.len()
            ),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(vec![top_line, filter_line(&state.query, Color::Magenta)]),
        chunks[0],
    );

    // 2. 档位列表渲染
    if state.filtered_variants.is_empty() {
        let empty_msg = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    "No variants match the filter.",
                    Style::default().fg(Color::Yellow),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(empty_msg), chunks[1]);
    } else {
        let mut lines = Vec::new();
        // 表头
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<28}", "Variant / Effort"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("Status", Style::default().fg(Color::DarkGray)),
        ]));

        let (start_idx, visible) = visible_window(
            state.selected_index,
            chunks[1].height.saturating_sub(1) as usize,
        );

        for (i, item) in state
            .filtered_variants
            .iter()
            .enumerate()
            .skip(start_idx)
            .take(visible)
        {
            let is_selected = i == state.selected_index;
            let is_active = item.id.as_ref() == state.active_variant.as_ref();

            let prefix = if is_selected {
                "> "
            } else if is_active {
                "* "
            } else {
                "  "
            };

            let row_style = if is_selected {
                selected_row_style(Color::Magenta)
            } else if is_active {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let status_str = if is_active { "[active]" } else { "-" };

            let status_style = if is_selected {
                selected_row_style(Color::Magenta)
            } else if is_active {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<28}", item.display_name), row_style),
                Span::styled(status_str, status_style),
            ]));
        }

        frame.render_widget(Paragraph::new(lines), chunks[1]);
    }

    // 3. 底部操作指引
    let footer = footer_hints(&[
        (" [↑/↓/j/k] ", Style::default().fg(Color::Magenta), "Navigate  "),
        (
            " [Enter] ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            "Set Variant  ",
        ),
        (" [type] ", Style::default().fg(Color::Magenta), "Filter  "),
        (" [Esc] ", Style::default().fg(Color::Yellow), "Cancel"),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}
