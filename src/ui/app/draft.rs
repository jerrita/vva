//! 流式草稿增量渲染缓存。
//!
//! 会话任务以写时复制发布草稿：未变部件共享同一 `Arc`，追加文本只克隆被修改
//! 的部件。渲染缓存以部件身份对齐已完成渲染的前缀，前缀部件不重渲染。
//!
//! 增长中的文本/思考部件按块边界提交：围栏内容逐行提交，围栏外提交到最后一个
//! 未闭合块之前（最后一个空行之后）。已提交行一次成型；尾区每帧重渲染，并在
//! 显示层剥除尾随空行，与全量渲染逐行一致。
//!
//! 块边界基于空行与围栏状态。跨块语义（前向引用定义等）在流式期间可能与全量
//! 渲染有出入，轮次落盘后由历史全量渲染校正。

use std::sync::Arc;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::model::{ContentPart, Role};
use crate::ui::markdown::{
    code_box_footer_line, code_box_header_line, code_box_line, render_markdown_open,
    strippable_line,
};
use crate::ui::views::chat::{
    ChatRender, PartContext, RenderState, ThoughtKind, append_md, append_part, part_context,
    polish_md_line, push_citations_block, push_empty_hint, thought_body_style, thought_kind,
    thought_prefix_style,
};

use super::pane::ThoughtHit;

#[derive(Default)]
pub(crate) struct DraftRenderCache {
    parts: Vec<RenderedPart>,
    /// WebSearchCall 行引用后续文本的注脚，变化时从首个搜索部件起重绘
    citations: Vec<(String, String)>,
}

struct RenderedPart {
    part: Arc<ContentPart>,
    line_end: usize,
    state: RenderState,
    scan: Option<BlockScan>,
}

impl DraftRenderCache {
    pub(crate) fn reset(&mut self) {
        self.parts.clear();
    }

    pub(crate) fn render(
        &mut self,
        render: &mut ChatRender<'_>,
        draft: &[Arc<ContentPart>],
        history_lines: usize,
        state: &mut RenderState,
        empty_session: bool,
    ) {
        let mut m = 0;
        while m < self.parts.len() && m < draft.len() {
            let rec = &self.parts[m];
            if rec.scan.is_some() || !Arc::ptr_eq(&rec.part, &draft[m]) {
                break;
            }
            m += 1;
        }
        let citations: Vec<(String, String)> = search_urls(draft);
        if self.citations != citations {
            self.citations = citations;
            if let Some(index) = draft
                .iter()
                .position(|part| matches!(&**part, ContentPart::WebSearchCall(_)))
            {
                m = m.min(index);
            }
        }
        let resume = self.parts.get(m).and_then(|rec| {
            (rec.scan.is_some() && m + 1 == draft.len() && part_extends(&rec.part, &draft[m]))
                .then(|| rec.scan.clone())
        });
        let line_keep = if resume.is_some() {
            self.parts[m].line_end
        } else {
            match m.checked_sub(1) {
                Some(prev) => self.parts[prev].line_end,
                None => history_lines,
            }
        };
        let mut state_at = match m.checked_sub(1) {
            Some(prev) => self.parts[prev].state,
            None => *state,
        };
        render.lines.truncate(line_keep);
        render.links.retain(|link| link.line < line_keep);
        render.hits.retain(|hit| hit.header_start < line_keep);
        render
            .thought_hits
            .retain(|hit| hit.header_start < line_keep);
        render
            .citations_hits
            .retain(|hit| hit.header_start < line_keep);
        self.parts.truncate(m);

        if draft.is_empty() {
            if empty_session {
                push_empty_hint(render);
            }
            *state = state_at;
            return;
        }
        let ctx = part_context(draft.iter().map(|part| &**part));
        for (index, part) in draft.iter().enumerate().skip(m) {
            let last = index + 1 == draft.len();
            let mut rec = RenderedPart {
                part: part.clone(),
                line_end: render.lines.len(),
                state: state_at,
                scan: if index == m {
                    resume.clone().flatten()
                } else {
                    None
                },
            };
            if last && streamable(render, &state_at, part) {
                stream_part(render, part, &ctx, &mut state_at, &mut rec);
            } else {
                append_part(render, Role::Assistant, part, &ctx, &mut state_at);
                rec.line_end = render.lines.len();
                rec.scan = None;
            }
            rec.state = state_at;
            self.parts.push(rec);
        }
        *state = state_at;
    }
}

fn search_urls(draft: &[Arc<ContentPart>]) -> Vec<(String, String)> {
    part_context(draft.iter().map(|part| &**part))
        .citations
        .into_iter()
        .map(|(url, title)| (url.to_string(), title.to_string()))
        .collect()
}

fn part_extends(old: &ContentPart, new: &ContentPart) -> bool {
    match (old, new) {
        (ContentPart::Text { text: before, .. }, ContentPart::Text { text: after, .. }) => {
            after.starts_with(before.as_str())
        }
        (
            ContentPart::Reasoning {
                content: before, ..
            },
            ContentPart::Reasoning { content: after, .. },
        ) => after.starts_with(before.as_str()),
        _ => false,
    }
}

fn streamable(render: &ChatRender<'_>, state: &RenderState, part: &ContentPart) -> bool {
    match part {
        ContentPart::Text { .. } => true,
        ContentPart::Reasoning { content, .. } => {
            matches!(thought_kind(render, state, content), ThoughtKind::Expanded)
        }
        _ => false,
    }
}

fn stream_part(
    render: &mut ChatRender<'_>,
    part: &Arc<ContentPart>,
    ctx: &PartContext<'_>,
    state: &mut RenderState,
    rec: &mut RenderedPart,
) {
    let resuming = rec.scan.is_some();
    let mut scan = rec.scan.take().unwrap_or(BlockScan {
        committed: 0,
        fence: None,
        md_start: 0,
    });
    match &**part {
        ContentPart::Text { text, annotations } => {
            state.prev_reasoning = false;
            state.prev_text = true;
            if !resuming {
                render.lines.push(Line::from(""));
                rec.line_end = render.lines.len();
                scan.md_start = render.lines.len();
            }
            stream_markdown(render, text, &mut scan, rec, None);
            rec.scan = Some(scan);
            strip_tail(render.lines, rec.line_end);
            push_citations_block(render, ctx, annotations, state);
        }
        ContentPart::Reasoning { content, .. } => {
            let group_start = !state.prev_reasoning;
            state.prev_reasoning = true;
            state.prev_text = false;
            if !resuming {
                if group_start {
                    render.lines.push(Line::from(""));
                    state.thought_groups += 1;
                    state.thought_expanded = true;
                    state.thought_inline = false;
                    let header_start = render.lines.len();
                    render.lines.push(Line::from(vec![Span::styled(
                        "- Thought",
                        thought_prefix_style(),
                    )]));
                    render.thought_hits.push(ThoughtHit {
                        id: format!("thought-{}", state.thought_groups),
                        header_start,
                        header_end: header_start + 1,
                        body_start: header_start + 1,
                        body_end: header_start + 1,
                    });
                }
                rec.line_end = render.lines.len();
                scan.md_start = render.lines.len();
            }
            stream_markdown(render, content, &mut scan, rec, Some(thought_body_style()));
            rec.scan = Some(scan);
            strip_tail(render.lines, rec.line_end);
            if let Some(hit) = render.thought_hits.last_mut() {
                hit.body_end = render.lines.len();
            }
        }
        _ => {}
    }
}

fn stream_markdown(
    render: &mut ChatRender<'_>,
    text: &str,
    scan: &mut BlockScan,
    rec: &mut RenderedPart,
    body_style: Option<Style>,
) {
    if scan.committed > text.len() {
        *scan = BlockScan {
            committed: 0,
            fence: None,
            md_start: scan.md_start,
        };
    }
    let max_width = render.max_width;
    let md_start = scan.md_start;
    let mut pos = scan.committed;
    loop {
        let Some(fence) = scan.fence.clone() else {
            let (boundary, fence_open) = block_boundary(text, pos);
            if boundary > pos {
                let preceded = render.lines.len() > md_start;
                let (lines, links) =
                    render_markdown_open(&text[pos..boundary], max_width, preceded);
                append_md(render, lines, links, body_style);
                rec.line_end = render.lines.len();
                scan.committed = boundary;
                pos = boundary;
            }
            match fence_open {
                Some(opened) => scan.fence = Some(opened),
                None => {
                    let preceded = render.lines.len() > md_start;
                    let (lines, links) = render_markdown_open(&text[pos..], max_width, preceded);
                    append_md(render, lines, links, body_style);
                    return;
                }
            }
            continue;
        };
        if pos == fence.start {
            if render.lines.len() > md_start {
                render.lines.push(Line::from(""));
            }
            render
                .lines
                .push(code_box_header_line(&fence.info, max_width));
            finish_stream_line(render, body_style);
            rec.line_end = render.lines.len();
            match text[pos..].find('\n') {
                Some(nl) => {
                    pos += nl + 1;
                    scan.committed = pos;
                }
                None => {
                    render.lines.push(code_box_footer_line(max_width));
                    finish_stream_line(render, body_style);
                    render.lines.push(Line::from(""));
                    return;
                }
            }
        }
        let mut closed = false;
        while let Some(nl) = text[pos..].find('\n') {
            let line_end = pos + nl;
            let next = line_end + 1;
            let line = strip_cr(&text[pos..line_end]);
            if is_closing_fence(line, fence.ch, fence.len) {
                render.lines.push(code_box_line("", max_width));
                finish_stream_line(render, body_style);
                render.lines.push(code_box_footer_line(max_width));
                finish_stream_line(render, body_style);
                render.lines.push(Line::from(""));
                scan.fence = None;
                scan.committed = next;
                rec.line_end = render.lines.len();
                pos = next;
                closed = true;
                break;
            }
            render
                .lines
                .push(code_box_line(dedent(line, fence.indent), max_width));
            finish_stream_line(render, body_style);
            scan.committed = next;
            rec.line_end = render.lines.len();
            pos = next;
        }
        if closed {
            continue;
        }
        let rest = strip_cr(&text[pos..]);
        if !rest.is_empty() && !is_closing_fence(rest, fence.ch, fence.len) {
            render
                .lines
                .push(code_box_line(dedent(rest, fence.indent), max_width));
            finish_stream_line(render, body_style);
        }
        render.lines.push(code_box_footer_line(max_width));
        finish_stream_line(render, body_style);
        render.lines.push(Line::from(""));
        return;
    }
}

fn finish_stream_line(render: &mut ChatRender<'_>, body_style: Option<Style>) {
    if let Some(line) = render.lines.last_mut() {
        polish_md_line(line, body_style);
    }
}

fn strip_tail(lines: &mut Vec<Line<'static>>, floor: usize) {
    while lines.len() > floor && lines.last().is_some_and(strippable_line) {
        lines.pop();
    }
}

#[derive(Clone)]
struct BlockScan {
    committed: usize,
    fence: Option<Fence>,
    md_start: usize,
}

#[derive(Clone)]
struct Fence {
    ch: char,
    len: usize,
    indent: usize,
    info: String,
    start: usize,
}

fn block_boundary(text: &str, from: usize) -> (usize, Option<Fence>) {
    let mut boundary = from;
    let mut pos = from;
    while let Some(nl) = text[pos..].find('\n') {
        let line_end = pos + nl;
        let next = line_end + 1;
        let line = strip_cr(&text[pos..line_end]);
        if let Some((ch, len, indent, info)) = opening_fence(line) {
            return (
                pos,
                Some(Fence {
                    ch,
                    len,
                    indent,
                    info,
                    start: pos,
                }),
            );
        }
        if line.trim().is_empty() {
            boundary = next;
        }
        pos = next;
    }
    (boundary, None)
}

fn opening_fence(line: &str) -> Option<(char, usize, usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = rest.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = rest.chars().take_while(|&c| c == ch).count();
    if len < 3 {
        return None;
    }
    let info = rest[len..].trim().to_string();
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, len, indent, info))
}

fn is_closing_fence(line: &str, ch: char, len: usize) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let count = rest.chars().take_while(|&c| c == ch).count();
    count >= len && rest[count..].trim().is_empty()
}

fn dedent(line: &str, indent: usize) -> &str {
    let mut n = 0;
    while n < indent && line[n..].starts_with(' ') {
        n += 1;
    }
    &line[n..]
}

fn strip_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}
