use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::skills::{Skill, SkillSource};
use crate::ui::modals::SkillsModalState;
use crate::ui::text::{pad_to_width, truncate};
use crate::ui::views::modal_shell::{
    ModalShellSpec, filter_line, footer_hints, list_row_affix, render_modal_list,
    render_modal_shell,
};

/// 渲染 /skills 模态弹窗：只读浏览已发现的 Agent Skills；
/// 返回列表区几何供鼠标命中测试
pub fn render_skills_modal(state: &SkillsModalState, frame: &mut Frame, area: Rect) -> Rect {
    let list_area = render_modal_shell(
        frame,
        area,
        ModalShellSpec {
            title: " Agent Skills ",
            accent: Color::Cyan,
            width: 86,
            height: 18,
            header_height: 2,
        },
        vec![
            Line::from(Span::styled(
                "  Skills are loaded by the model via the Skill tool when a task matches.",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            filter_line(&state.query, Color::Cyan),
        ],
        footer_hints(&[
            (" [↑↓] ", Style::default().fg(Color::Cyan), "Nav  "),
            (" [Esc] ", Style::default().fg(Color::Yellow), "Close"),
        ]),
    );

    let source_width = 9;
    let name_width = state
        .filtered_skills
        .iter()
        .map(|skill| skill.name.width())
        .max()
        .unwrap_or(0)
        .max("Name".width())
        .min(24);
    let fixed_width = 2 + name_width + source_width;
    let desc_width = (list_area.width as usize)
        .saturating_sub(fixed_width)
        .saturating_sub(4);
    let gap = " ".repeat(2);
    let right_padding = " ".repeat(2);

    let header = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            pad_to_width("Name", name_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(gap.clone()),
        Span::styled(
            pad_to_width("Source", source_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(gap.clone()),
        Span::styled("Description", Style::default().fg(Color::DarkGray)),
        Span::raw(right_padding.clone()),
    ]);

    render_modal_list(
        frame,
        list_area,
        &state.filtered_skills,
        state.selected_index,
        header,
        Some((
            "No skills discovered.",
            Some(
                "Place skills in ~/.vva/skills, ~/.agents/skills, <workspace>/.vva/skills or <workspace>/.agents/skills, each with a SKILL.md.",
            ),
        )),
        |i, skill: &Skill| {
            let is_selected = i == state.selected_index;
            let (prefix, row_style) = list_row_affix(is_selected, false, Color::Cyan);
            let source = match skill.source {
                SkillSource::Global => "global",
                SkillSource::Project => "project",
            };
            let source_style = if is_selected {
                row_style
            } else {
                Style::default().fg(Color::DarkGray)
            };

            Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(
                    pad_to_width(&truncate(&skill.name, name_width), name_width),
                    row_style,
                ),
                Span::styled(gap.clone(), row_style),
                Span::styled(pad_to_width(source, source_width), source_style),
                Span::styled(gap.clone(), row_style),
                Span::styled(truncate(&skill.description, desc_width), row_style),
                Span::styled(right_padding.clone(), row_style),
            ])
        },
    );

    list_area
}
