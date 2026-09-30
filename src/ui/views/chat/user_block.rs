//! 用户消息引用块渲染。

use super::*;

pub(super) const USER_BLOCK_BG: Color = Color::Rgb(28, 32, 40);
pub(super) const USER_BLOCK_BAR: Color = Color::Rgb(96, 149, 239);
pub(super) const SELECTION_STYLE: Style = Style::new().fg(Color::White).bg(Color::Rgb(60, 90, 140));

pub(super) fn user_block_row(spans: Vec<Span<'static>>, max_width: usize) -> Line<'static> {
    let bar_style = Style::default().fg(USER_BLOCK_BAR).bg(USER_BLOCK_BG);
    let base = Style::default().bg(USER_BLOCK_BG);
    let mut width = 1;
    let mut out = vec![Span::styled("▌", bar_style)];
    for span in spans {
        width += span.content.width();
        out.push(span);
    }
    if max_width > width {
        out.push(Span::styled(" ".repeat(max_width - width), base));
    }
    Line::from(out)
}

/// 以左侧色条引用块渲染用户消息（含上下留白行），与模型输出形成区块级区分
pub(super) fn push_user_block(lines: &mut Vec<Line<'static>>, text: &str, max_width: usize) {
    let text_style = Style::default()
        .fg(Color::Rgb(226, 229, 234))
        .bg(USER_BLOCK_BG);
    let inner_width = max_width.saturating_sub(3);
    lines.push(user_block_row(Vec::new(), max_width));
    for l in text.lines() {
        let spans = vec![Span::styled(l.to_string(), text_style)];
        for line in wrap_spans_to_lines(&spans, inner_width, "   ", "   ") {
            let mut spans = line.spans;
            if let Some(indent) = spans.first_mut() {
                indent.style = text_style;
            }
            lines.push(user_block_row(spans, max_width));
        }
    }
    lines.push(user_block_row(Vec::new(), max_width));
}

/// 用户轮次附件的紧凑引用行：色条引用块内展示（超宽折行），不展开附件正文
pub(super) fn push_user_attachment_row(
    lines: &mut Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    max_width: usize,
) {
    let text_style = Style::default()
        .fg(Color::Rgb(226, 229, 234))
        .bg(USER_BLOCK_BG);
    let inner_width = max_width.saturating_sub(3);
    for line in wrap_spans_to_lines(&spans, inner_width, "   ", "   ") {
        let mut spans = line.spans;
        if let Some(indent) = spans.first_mut() {
            indent.style = text_style;
        }
        lines.push(user_block_row(spans, max_width));
    }
}
