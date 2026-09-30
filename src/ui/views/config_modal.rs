use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::modals::{ConfigItemKind, ConfigModalState};
use crate::ui::views::modal_shell::{
    ModalShellSpec, filter_line, footer_hints, list_row_affix, render_modal_list,
    render_modal_shell, selected_row_style,
};
use crate::ui::views::{bold, fg};

/// 渲染 /config 模态弹窗，支持上下键或 j/k 浏览与调整各项会话配置（模型、推理档位、网络搜索等）；
/// 返回列表区几何供鼠标命中测试
pub fn render_config_modal(state: &ConfigModalState, frame: &mut Frame, area: Rect) -> Rect {
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
            (" [↑/↓/j/k] ", fg(Color::Cyan), "Navigate  "),
            (" [Enter/Space] ", bold(Color::Green), "Modify / Toggle  "),
            (" [type] ", fg(Color::Cyan), "Filter  "),
            (" [Esc] ", fg(Color::Yellow), "Close"),
        ]),
    );

    let header = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{:<24}", "Setting"), fg(Color::DarkGray)),
        Span::styled(format!("{:<20}", "Current Value"), fg(Color::DarkGray)),
        Span::styled("Action", fg(Color::DarkGray)),
    ]);

    render_modal_list(
        frame,
        list_area,
        &state.filtered_items,
        state.selected_index,
        header,
        None,
        |i, item| {
            let (prefix, row_style) = list_row_affix(i == state.selected_index, false, Color::Cyan);

            let val_style = if i == state.selected_index {
                selected_row_style(Color::Cyan)
            } else {
                bold(Color::Yellow)
            };

            let action_str = match item.kind {
                ConfigItemKind::Model => "Enter to select model",
                ConfigItemKind::Variant => "Enter to select variant",
                ConfigItemKind::WebSearch => "Enter/Space to toggle",
                ConfigItemKind::ThinkingDisplay => "Enter/Space to toggle",
            };

            Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<24}", item.name), row_style),
                Span::styled(format!("{:<20}", item.current_value), val_style),
                Span::styled(
                    action_str,
                    if i == state.selected_index {
                        row_style
                    } else {
                        fg(Color::DarkGray)
                    },
                ),
            ])
        },
    );

    list_area
}
