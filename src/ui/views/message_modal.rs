use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};

use crate::ui::modals::{ForkTitleState, MESSAGE_ACTIONS, MessageActionState};
use crate::ui::views::modal_shell::{
    ModalShellSpec, footer_hints, list_row_affix, render_modal_list, render_modal_shell,
};
use crate::ui::views::{bold, fg};

/// 渲染用户消息 Revert / Fork 选择弹窗，返回列表区供鼠标命中
pub fn render_message_action_modal(
    state: &MessageActionState,
    frame: &mut Frame,
    area: Rect,
) -> Rect {
    let preview = crate::ui::text::truncate(&state.prompt.replace('\n', " "), 48);
    let list_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Message ",
            accent: Color::Cyan,
            width: 64,
            height: 11,
            header_height: 2,
        },
        vec![Line::from(vec![
            Span::styled("  Prompt: ", fg(Color::DarkGray)),
            Span::styled(preview, fg(Color::White)),
        ])],
        footer_hints(&[
            (" [↑/↓/j/k] ", fg(Color::Cyan), "Navigate  "),
            (" [Enter] ", bold(Color::Green), "Select  "),
            (" [Esc] ", fg(Color::Yellow), "Cancel"),
        ]),
    );

    render_modal_list(
        frame,
        list_area,
        &MESSAGE_ACTIONS,
        state.selected_index,
        Line::from(""),
        None,
        |i, item| {
            let (prefix, row_style) = list_row_affix(i == state.selected_index, false, Color::Cyan);
            Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("{:<8}", item.0), row_style),
                Span::styled(item.1.to_string(), row_style),
            ])
        },
    );
    list_area
}

/// 渲染 Fork 标题编辑弹窗
pub fn render_fork_title_modal(state: &ForkTitleState, frame: &mut Frame, area: Rect) {
    render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Fork Session ",
            accent: Color::Cyan,
            width: 60,
            height: 8,
            header_height: 2,
        },
        vec![
            Line::from(Span::styled("  New session title:", fg(Color::DarkGray))),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(state.title.text.clone(), bold(Color::Cyan)),
                Span::styled("_", fg(Color::Cyan)),
            ]),
        ],
        Line::from(vec![
            Span::styled(" [Enter] ", fg(Color::Green)),
            Span::styled("Fork  ", fg(Color::DarkGray)),
            Span::styled(" [Esc] ", fg(Color::Yellow)),
            Span::styled("Back", fg(Color::DarkGray)),
        ]),
    );
}
