use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::modals::VariantsModalState;
use crate::ui::views::modal_shell::{
    ModalShellSpec, filter_line, footer_hints, list_row_affix, render_modal_list,
    render_modal_shell, selected_row_style,
};

/// 渲染 /variant 模态弹窗，支持上下键或 j/k 浏览选择当前模型可用的推理强度档位；
/// 返回列表区几何供鼠标命中测试
pub fn render_variants_modal(state: &VariantsModalState, frame: &mut Frame, area: Rect) -> Rect {
    // 顶部状态指示行
    let curr_model = &state.current_model;
    let curr_var = state.active_variant.as_deref().unwrap_or("default");
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

    let list_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Select Reasoning Variant (↑/↓ to navigate, Enter to switch) ",
            accent: Color::Magenta,
            width: 60,
            height: 16,
            header_height: 3,
        },
        vec![top_line, filter_line(&state.query, Color::Magenta)],
        footer_hints(&[
            (
                " [↑/↓/j/k] ",
                Style::default().fg(Color::Magenta),
                "Navigate  ",
            ),
            (
                " [Enter] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "Set Variant  ",
            ),
            (" [type] ", Style::default().fg(Color::Magenta), "Filter  "),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Cancel"),
        ]),
    );

    // 表头
    let header = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{:<28}", "Variant / Effort"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("Status", Style::default().fg(Color::DarkGray)),
    ]);

    render_modal_list(
        frame,
        list_area,
        &state.filtered_variants,
        state.selected_index,
        header,
        Some(("No variants match the filter.", None)),
        |i, item| {
            let is_selected = i == state.selected_index;
            let is_active = item.id.as_ref() == state.active_variant.as_ref();

            let (prefix, row_style) = list_row_affix(is_selected, is_active, Color::Magenta);

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

            Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<28}", item.display_name), row_style),
                Span::styled(status_str, status_style),
            ])
        },
    );

    list_area
}
