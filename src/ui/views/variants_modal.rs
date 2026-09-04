use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::app::App;

/// 渲染 /variant 模态弹窗，支持上下键或 j/k 浏览选择当前模型可用的推理强度档位
pub fn render_variants_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.variants_modal {
        Some(s) => s,
        None => return,
    };

    // 居中计算弹出框尺寸
    let modal_width = 60.min(area.width.saturating_sub(4));
    let modal_height = 16.min(area.height.saturating_sub(4));

    let modal_rect = Rect {
        x: (area.width - modal_width) / 2,
        y: (area.height - modal_height) / 2,
        width: modal_width,
        height: modal_height,
    };

    // 清空背景
    frame.render_widget(Clear, modal_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(
            " Select Reasoning Variant (↑/↓ to navigate, Enter to switch) ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));

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
    let search_line = Line::from(vec![
        Span::styled("  Filter: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&state.query, Style::default().fg(Color::Magenta)),
        Span::styled("_", Style::default().fg(Color::Magenta)),
    ]);
    frame.render_widget(Paragraph::new(vec![top_line, search_line]), chunks[0]);

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

        let list_visible_height = chunks[1].height.saturating_sub(1) as usize;
        let start_idx = if state.selected_index >= list_visible_height {
            state.selected_index - list_visible_height + 1
        } else {
            0
        };

        for (i, item) in state
            .filtered_variants
            .iter()
            .enumerate()
            .skip(start_idx)
            .take(list_visible_height)
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
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else if is_active {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let status_str = if is_active { "[active]" } else { "-" };

            let status_style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
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

        let list_p = Paragraph::new(lines);
        frame.render_widget(list_p, chunks[1]);
    }

    // 3. 底部操作指引
    let footer_line = Line::from(vec![
        Span::styled(" [↑/↓/j/k] ", Style::default().fg(Color::Magenta)),
        Span::styled("Navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " [Enter] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Set Variant  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [type] ", Style::default().fg(Color::Magenta)),
        Span::styled("Filter  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [Esc] ", Style::default().fg(Color::Yellow)),
        Span::styled("Cancel", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer_line), chunks[2]);
}
