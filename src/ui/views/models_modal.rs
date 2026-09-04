use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::app::App;
use crate::ui::text::truncate;

/// 渲染 /model 模态弹窗，支持上下键或 j/k 浏览选择当前配置中的可用模型
pub fn render_models_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.models_modal {
        Some(s) => s,
        None => return,
    };

    // 居中计算弹出框尺寸
    let modal_width = 90.min(area.width.saturating_sub(4));
    let modal_height = (state.filtered_models.len() as u16 + 6).min(area.height.saturating_sub(4));

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
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Select Model (↑/↓ to navigate, Enter to switch) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // 当前会话活跃模型指示与搜索条件
            Constraint::Min(1),    // 模型列表
            Constraint::Length(1), // 底部操作按键提示
        ])
        .split(inner);

    // 1. 顶部当前状态行
    let curr_model = app.current_session_model();
    let top_line = Line::from(vec![
        Span::raw("  "),
        Span::styled("Currently Active: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            curr_model,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "({}/{} models)",
                state.filtered_models.len(),
                state.models.len()
            ),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);
    let search_line = Line::from(vec![
        Span::styled("  Filter: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&state.query, Style::default().fg(Color::Cyan)),
        Span::styled("_", Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(vec![top_line, search_line]), chunks[0]);

    // 2. 模型列表渲染
    if state.filtered_models.is_empty() {
        let empty_msg = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    "No models match the filter.",
                    Style::default().fg(Color::Yellow),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(empty_msg), chunks[1]);
    } else {
        let mut lines = Vec::new();
        let show_details = chunks[1].width >= 70;
        let provider_width: usize = if show_details { 14 } else { 12 };
        let protocol_width: usize = 10;
        let context_width: usize = 10;
        let fixed_width =
            2 + provider_width + protocol_width + if show_details { context_width } else { 0 };
        let name_width = (chunks[1].width as usize)
            .saturating_sub(fixed_width)
            .min(22);
        let variants_width = if show_details {
            (chunks[1].width as usize).saturating_sub(fixed_width + name_width)
        } else {
            0
        };

        // 表头
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<name_width$}", "Name"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<provider_width$}", "Provider"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<protocol_width$}", "Protocol"),
                Style::default().fg(Color::DarkGray),
            ),
            if show_details {
                Span::styled(
                    format!("{:<context_width$}", "Context"),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                Span::raw("")
            },
            if show_details {
                Span::styled("Variants", Style::default().fg(Color::DarkGray))
            } else {
                Span::raw("")
            },
        ]));

        let list_visible_height = chunks[1].height.saturating_sub(1) as usize;
        let start_idx = if state.selected_index >= list_visible_height {
            state.selected_index - list_visible_height + 1
        } else {
            0
        };

        for (i, item) in state
            .filtered_models
            .iter()
            .enumerate()
            .skip(start_idx)
            .take(list_visible_height)
        {
            let is_selected = i == state.selected_index;
            let is_active = item.full_id == app.current_session_model();

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
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_active {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let short_name = truncate(&item.display_name, name_width);
            let short_provider = truncate(&item.provider_id, provider_width);

            let context_str = match item.context_limit {
                Some(ctx) if ctx >= 1_000_000 => format!("{}M", ctx / 1_000_000),
                Some(ctx) if ctx >= 1_000 => format!("{}k", ctx / 1_000),
                Some(ctx) => ctx.to_string(),
                None => "-".to_string(),
            };

            let variants_str = match &item.variants {
                Some(vars) if !vars.is_empty() => vars.join(", "),
                _ => "-".to_string(),
            };

            let protocol_style = if is_selected {
                row_style
            } else {
                Style::default().fg(match item.provider_type.as_str() {
                    "response" => Color::Green,
                    "anthropic" => Color::Magenta,
                    "gemini" => Color::Blue,
                    _ => Color::DarkGray,
                })
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<name_width$}", short_name), row_style),
                Span::styled(format!("{:<provider_width$}", short_provider), row_style),
                Span::styled(
                    format!(
                        "{:<protocol_width$}",
                        truncate(&item.provider_type, protocol_width)
                    ),
                    protocol_style,
                ),
                if show_details {
                    Span::styled(format!("{:<context_width$}", context_str), row_style)
                } else {
                    Span::raw("")
                },
                if show_details {
                    Span::styled(truncate(&variants_str, variants_width), row_style)
                } else {
                    Span::raw("")
                },
            ]));
        }

        let list_p = Paragraph::new(lines);
        frame.render_widget(list_p, chunks[1]);
    }

    // 3. 底部操作指引
    let footer_line = Line::from(vec![
        Span::styled(" [↑/↓/j/k] ", Style::default().fg(Color::Cyan)),
        Span::styled("Navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " [Enter] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Switch Model  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [type] ", Style::default().fg(Color::Cyan)),
        Span::styled("Filter  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [Esc] ", Style::default().fg(Color::Yellow)),
        Span::styled("Cancel", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer_line), chunks[2]);
}
