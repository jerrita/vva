use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::app::App;

/// 渲染 /model 模态弹窗，支持上下键或 j/k 浏览选择当前配置中的可用模型
pub fn render_models_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.models_modal {
        Some(s) => s,
        None => return,
    };

    // 居中计算弹出框尺寸
    let modal_width = 88.min(area.width.saturating_sub(4));
    let modal_height = 20.min(area.height.saturating_sub(4));

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
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // 当前会话活跃模型指示
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
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("({} models configured)", state.models.len()),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        ),
    ]);
    frame.render_widget(Paragraph::new(top_line), chunks[0]);

    // 2. 模型列表渲染
    if state.models.is_empty() {
        let empty_msg = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    "No models configured in ~/.vva/config.jsonc or project config.",
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
            Span::styled(format!("{:<26}", "Model Spec"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:<20}", "Display Name"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:<12}", "Protocol"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:<10}", "Context"), Style::default().fg(Color::DarkGray)),
            Span::styled("Variants", Style::default().fg(Color::DarkGray)),
        ]));

        let list_visible_height = chunks[1].height.saturating_sub(1) as usize;
        let start_idx = if state.selected_index >= list_visible_height {
            state.selected_index - list_visible_height + 1
        } else {
            0
        };

        for (i, item) in state.models.iter().enumerate().skip(start_idx).take(list_visible_height) {
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
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let short_spec = if item.full_id.len() > 24 {
                format!("{}..", &item.full_id[..22])
            } else {
                item.full_id.clone()
            };

            let short_name = if item.display_name.len() > 18 {
                format!("{}..", &item.display_name[..16])
            } else {
                item.display_name.clone()
            };

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

            let protocol_color = if is_selected {
                Color::Black
            } else {
                match item.provider_type.as_str() {
                    "response" => Color::Green,
                    "anthropic" => Color::Magenta,
                    "gemini" => Color::Blue,
                    _ => Color::DarkGray,
                }
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<26}", short_spec), row_style),
                Span::styled(format!("{:<20}", short_name), row_style),
                Span::styled(format!("{:<12}", item.provider_type), Style::default().fg(protocol_color)),
                Span::styled(format!("{:<10}", context_str), row_style),
                Span::styled(variants_str, row_style),
            ]));
        }

        let list_p = Paragraph::new(lines);
        frame.render_widget(list_p, chunks[1]);
    }

    // 3. 底部操作指引
    let footer_line = Line::from(vec![
        Span::styled(" [↑/↓/j/k] ", Style::default().fg(Color::Cyan)),
        Span::styled("Navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [Enter] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::styled("Switch Model  ", Style::default().fg(Color::DarkGray)),
        Span::styled(" [Esc/q] ", Style::default().fg(Color::Yellow)),
        Span::styled("Cancel", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(footer_line), chunks[2]);
}

