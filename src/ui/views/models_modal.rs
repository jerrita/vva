use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::app::App;
use crate::ui::text::truncate;
use crate::ui::views::modal_shell::{
    ModalShellSpec, empty_state, filter_line, footer_hints, list_row_affix, render_modal_shell,
    visible_window,
};

/// 渲染 /model 模态弹窗，支持上下键或 j/k 浏览选择当前配置中的可用模型
pub fn render_models_modal(app: &App, frame: &mut Frame, area: Rect) {
    let state = match &app.models_modal {
        Some(s) => s,
        None => return,
    };

    // 顶部当前状态行
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

    // 弹窗高度随条目数自适应
    let list_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Select Model (↑/↓ to navigate, Enter to switch) ",
            accent: Color::Cyan,
            width: 90,
            height: state.filtered_models.len() as u16 + 6,
            header_height: 2,
        },
        vec![top_line, filter_line(&state.query, Color::Cyan)],
        footer_hints(&[
            (
                " [↑/↓/j/k] ",
                Style::default().fg(Color::Cyan),
                "Navigate  ",
            ),
            (
                " [Enter] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "Switch Model  ",
            ),
            (" [type] ", Style::default().fg(Color::Cyan), "Filter  "),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Cancel"),
        ]),
    );

    // 模型列表渲染
    if state.filtered_models.is_empty() {
        frame.render_widget(
            Paragraph::new(empty_state("No models match the filter.", None)),
            list_area,
        );
        return;
    }

    let mut lines = Vec::new();
    let show_details = list_area.width >= 70;
    let provider_width: usize = if show_details { 14 } else { 12 };
    let protocol_width: usize = 10;
    let context_width: usize = 10;
    let fixed_width =
        2 + provider_width + protocol_width + if show_details { context_width } else { 0 };
    let name_width = (list_area.width as usize)
        .saturating_sub(fixed_width)
        .min(22);
    let variants_width = if show_details {
        (list_area.width as usize).saturating_sub(fixed_width + name_width)
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

    let (start_idx, visible) = visible_window(state.selected_index, list_area.height as usize);

    for (i, item) in state
        .filtered_models
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(visible)
    {
        let is_selected = i == state.selected_index;
        let is_active = item.full_id == app.current_session_model();

        let (prefix, row_style) = list_row_affix(is_selected, is_active, Color::Cyan);

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
                "response" | "glm-coding-plan" => Color::Green,
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

    frame.render_widget(Paragraph::new(lines), list_area);
}
