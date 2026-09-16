//! 思考（reasoning）内容渲染：内联/折叠启发式。

use super::state::*;
use super::*;

pub(super) const THOUGHT_INLINE_PREFIX: &str = "Thought: ";

pub(super) fn line_display_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

pub(super) fn thought_is_prose(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let stem = text
        .strip_suffix("...")
        .or_else(|| text.strip_suffix('…'))
        .unwrap_or(text);
    matches!(
        stem.chars().last(),
        Some('.' | '!' | '?' | '。' | '！' | '？')
    ) || text.contains(". ")
        || text.contains("! ")
        || text.contains("? ")
        || text.contains('。')
}

pub(super) fn thought_is_title(content: &str, max_width: usize) -> bool {
    if content.lines().count() > 1 || thought_is_prose(content) {
        return false;
    }
    let prefix_w = THOUGHT_INLINE_PREFIX.width();
    let (mut body, _) = render_markdown(content, max_width.saturating_sub(prefix_w));
    for line in &mut body {
        strip_all_leading_spaces(line);
    }
    match body.as_slice() {
        [] => true,
        [line] => prefix_w.saturating_add(line_display_width(line)) <= max_width.max(prefix_w),
        _ => false,
    }
}

pub(super) fn strip_all_leading_spaces(line: &mut Line<'static>) {
    while let Some(content) = line.spans.first().map(|span| span.content.as_ref()) {
        let trimmed = content.trim_start_matches(' ');
        if trimmed.is_empty() {
            line.spans.remove(0);
            continue;
        }
        if trimmed.len() != content.len() {
            line.spans[0].content = trimmed.to_string().into();
        }
        break;
    }
}

pub(super) fn push_inline_thought(
    render: &mut ChatRender<'_>,
    content: &str,
    prefix_style: Style,
    body_style: Style,
) {
    let prefix_w = THOUGHT_INLINE_PREFIX.width();
    let md_width = render.max_width.saturating_sub(prefix_w);
    let (mut body, mut body_links) = render_markdown(content, md_width);
    for line in &mut body {
        strip_all_leading_spaces(line);
        for span in &mut line.spans {
            span.style = span.style.patch(body_style);
        }
    }
    if body.is_empty() {
        body.push(Line::from(vec![Span::styled("Thought:", prefix_style)]));
    } else {
        let first = &mut body[0];
        let mut spans = vec![Span::styled("Thought:", prefix_style), Span::raw(" ")];
        spans.append(&mut first.spans);
        first.spans = spans;
        for line in &mut body[1..] {
            line.spans.insert(0, Span::raw(" ".repeat(prefix_w)));
        }
    }
    let line_offset = render.lines.len();
    for link in &mut body_links {
        link.line += line_offset;
        link.columns.start = link.columns.start.saturating_sub(3) + prefix_w;
        link.columns.end = link.columns.end.saturating_sub(3) + prefix_w;
    }
    render.links.extend(body_links);
    render.lines.extend(body);
}
