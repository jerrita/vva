use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::modals::ModelsModalState;
use crate::ui::text::truncate;
use crate::ui::views::modal_shell::{
    ModalShellSpec, filter_line, footer_hints, list_row_affix, render_modal_list,
    render_modal_shell,
};
use crate::ui::views::{bold, fg};

/// 渲染 /model 模态弹窗，支持上下键或 j/k 浏览选择当前配置中的可用模型；
/// 返回列表区几何供鼠标命中测试
pub fn render_models_modal(
    state: &ModelsModalState,
    current_model: &str,
    frame: &mut Frame,
    area: Rect,
) -> Rect {
    // 顶部当前状态行
    let top_line = Line::from(vec![
        Span::raw("  "),
        Span::styled("Currently Active: ", fg(Color::DarkGray)),
        Span::styled(current_model, bold(Color::Yellow)),
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
            (" [↑/↓/j/k] ", fg(Color::Cyan), "Navigate  "),
            (" [Enter] ", bold(Color::Green), "Switch Model  "),
            (" [type] ", fg(Color::Cyan), "Filter  "),
            (" [Esc] ", fg(Color::Yellow), "Cancel"),
        ]),
    );

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
    let header = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{:<name_width$}", "Name"), fg(Color::DarkGray)),
        Span::styled(
            format!("{:<provider_width$}", "Provider"),
            fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<protocol_width$}", "Protocol"),
            fg(Color::DarkGray),
        ),
        if show_details {
            Span::styled(
                format!("{:<context_width$}", "Context"),
                fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        },
        if show_details {
            Span::styled("Variants", fg(Color::DarkGray))
        } else {
            Span::raw("")
        },
    ]);

    render_modal_list(
        frame,
        list_area,
        &state.filtered_models,
        state.selected_index,
        header,
        Some(("No models match the filter.", None)),
        |i, item| {
            let is_selected = i == state.selected_index;
            let is_active = item.full_id == current_model;

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
                    "response" => Color::Green,
                    "anthropic" => Color::Magenta,
                    "gemini" => Color::Blue,
                    _ => Color::DarkGray,
                })
            };

            Line::from(vec![
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
            ])
        },
    );

    list_area
}
