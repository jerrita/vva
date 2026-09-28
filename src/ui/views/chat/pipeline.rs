//! 渲染管线：内容部件 → 终端行的主流程。

use super::*;
use super::{citations::*, state::*, thought::*, tool::*, user_block::*};

pub(super) fn worked_duration(elapsed_ms: u64) -> String {
    if elapsed_ms.is_multiple_of(1_000) {
        format!("{}s", elapsed_ms / 1_000)
    } else {
        format!("{:.1}s", elapsed_ms as f64 / 1_000.0)
    }
}

pub(crate) fn append_collapsed_response(
    render: &mut ChatRender<'_>,
    turn_id: &str,
    elapsed_ms: u64,
    state: &mut RenderState,
) {
    render.lines.push(Line::from(""));
    let line_start = render.lines.len();
    render.lines.push(Line::from(vec![Span::styled(
        format!("> Worked {}", worked_duration(elapsed_ms)),
        Style::default().fg(Color::Rgb(150, 150, 150)),
    )]));
    render.response_hits.push(ResponseHit {
        id: turn_id.to_string(),
        line_start,
        line_end: render.lines.len(),
        collapsed: true,
        collapsible: true,
    });
    state.prev_reasoning = false;
    state.prev_text = false;
    state.prev_web = false;
}

pub(crate) fn collect_tool_results(view: &SessionView) -> HashMap<String, &ToolResult> {
    let mut map = HashMap::new();
    let parts = view
        .session
        .turns
        .iter()
        .flat_map(|turn| turn.messages.iter().flat_map(|message| &message.parts))
        .chain(view.draft.iter().map(|part| &**part));
    for part in parts {
        if let ContentPart::ToolResult(result) = part {
            map.insert(result.tool_call_id.clone(), result);
        }
    }
    map
}

pub(crate) fn append_parts<'a>(
    render: &mut ChatRender<'_>,
    role: Role,
    parts: impl Iterator<Item = &'a ContentPart> + Clone,
    state: &mut RenderState,
) {
    let ctx = part_context(parts.clone());
    for part in parts {
        append_part(render, role, part, &ctx, state);
    }
}

pub(crate) fn part_context<'a>(
    parts: impl Iterator<Item = &'a ContentPart> + Clone,
) -> PartContext<'a> {
    PartContext {
        calls: parts
            .clone()
            .filter_map(|part| match part {
                ContentPart::WebSearchCall(call) => Some(call),
                _ => None,
            })
            .collect(),
        citations: search_citations(parts),
    }
}

pub(crate) fn thought_kind(
    render: &ChatRender<'_>,
    state: &RenderState,
    content: &str,
) -> ThoughtKind {
    if !state.prev_reasoning {
        if content.lines().count() <= 1
            && !thought_is_prose(content)
            && thought_is_title(content, render.max_width)
        {
            return ThoughtKind::Inline;
        }
        let expanded = render
            .thought_expand
            .get(&format!("thought-{}", state.thought_groups + 1))
            .copied()
            .unwrap_or(!render.thinking_concise);
        return if expanded {
            ThoughtKind::Expanded
        } else {
            ThoughtKind::Collapsed
        };
    }
    if state.thought_inline {
        ThoughtKind::Inline
    } else if state.thought_expanded {
        ThoughtKind::Expanded
    } else {
        ThoughtKind::Collapsed
    }
}

/// 思考正文与前缀的统一样式（历史渲染与流式草稿共用，保证两者一致）
pub(crate) fn thought_body_style() -> Style {
    Style::default()
        .fg(Color::Rgb(120, 120, 120))
        .add_modifier(Modifier::ITALIC)
}

pub(crate) fn thought_prefix_style() -> Style {
    Style::default()
        .fg(Color::Rgb(140, 140, 140))
        .add_modifier(Modifier::BOLD)
}

/// 单行 Markdown 成型：剥除排版缩进首空格并套用正文字体样式
pub(crate) fn polish_md_line(line: &mut Line<'static>, body_style: Option<Style>) {
    if let Some(span) = line.spans.first_mut()
        && span.content.starts_with(' ')
    {
        span.content.to_mut().remove(0);
    }
    if let Some(style) = body_style {
        for span in &mut line.spans {
            span.style = span.style.patch(style);
        }
    }
}

pub(crate) fn append_md(
    render: &mut ChatRender<'_>,
    mut md_lines: Vec<Line<'static>>,
    mut md_links: Vec<LinkRegion>,
    body_style: Option<Style>,
) {
    let offset = render.lines.len();
    for link in &mut md_links {
        link.line += offset;
        link.columns.start = link.columns.start.saturating_sub(1);
        link.columns.end = link.columns.end.saturating_sub(1);
    }
    for line in &mut md_lines {
        polish_md_line(line, body_style);
    }
    render.links.extend(md_links);
    render.lines.extend(md_lines);
}

pub(crate) fn append_part(
    render: &mut ChatRender<'_>,
    role: Role,
    part: &ContentPart,
    ctx: &PartContext<'_>,
    state: &mut RenderState,
) {
    let max_width = render.max_width;
    let citations = &ctx.citations;
    let prev_web = state.prev_web;
    state.prev_web = matches!(
        (role, part),
        (Role::Assistant, ContentPart::WebSearchCall(_))
    );
    match (role, part) {
        (Role::User, ContentPart::Text { text, .. }) => {
            if let Some(reminder) = crate::system::parse_agents_reminder(text) {
                match reminder.kind {
                    crate::system::AgentsReminderKind::Global
                    | crate::system::AgentsReminderKind::Project
                    | crate::system::AgentsReminderKind::Skills
                    | crate::system::AgentsReminderKind::BackgroundTask => {
                        // 全局、项目、技能清单与后台任务完成通知不展示（完成状态在工具块展示）
                    }
                    crate::system::AgentsReminderKind::Directory
                    | crate::system::AgentsReminderKind::SkillLoaded => {
                        // 按需自动加载的展示 Load xxx/AGENTS.md 与 Load <skill_name>
                        let loaded = if reminder.name.is_empty() {
                            reminder.path
                        } else {
                            reminder.name
                        };
                        let needs_separator = state.prev_reasoning || state.prev_text;
                        state.prev_reasoning = false;
                        state.prev_text = true;
                        if needs_separator {
                            render.lines.push(Line::from(""));
                        }
                        let spans = vec![Span::styled(
                            format!("Load {loaded}"),
                            Style::default().fg(Color::Rgb(150, 150, 150)),
                        )];
                        render
                            .lines
                            .extend(wrap_spans_to_lines(&spans, max_width, "", "   "));
                    }
                }
                return;
            }
            state.prev_reasoning = false;
            state.prev_text = false;
            render.lines.push(Line::from(""));
            push_user_block(render.lines, text, max_width);
        }
        (Role::Assistant, ContentPart::Reasoning { content, .. }) => {
            // 连续思考段归为一段，组间保留空行。
            let group_start = !state.prev_reasoning;
            let thought = group_start.then(|| thought_kind(render, state, content));
            state.prev_reasoning = true;
            state.prev_text = false;
            if group_start {
                render.lines.push(Line::from(""));
            }
            let body_style = thought_body_style();
            let prefix_style = thought_prefix_style();

            if group_start {
                let kind = thought.expect("group_start implies thought kind");
                state.thought_groups += 1;
                state.thought_inline = matches!(kind, ThoughtKind::Inline);
                state.thought_expanded = matches!(kind, ThoughtKind::Expanded);
                if state.thought_inline {
                    push_inline_thought(render, content, prefix_style, body_style);
                    return;
                }
                let marker = if state.thought_expanded {
                    "- Thought"
                } else {
                    "+ Thought"
                };
                let header_start = render.lines.len();
                render
                    .lines
                    .push(Line::from(vec![Span::styled(marker, prefix_style)]));
                render.thought_hits.push(ThoughtHit {
                    id: format!("thought-{}", state.thought_groups),
                    header_start,
                    header_end: header_start + 1,
                    body_start: header_start + 1,
                    body_end: header_start + 1,
                });
            }
            if state.thought_inline {
                return;
            }
            if state.thought_expanded {
                let (body, body_links) = render_markdown(content, max_width);
                append_md(render, body, body_links, Some(body_style));
                // 当前分组的命中必为最新压入的一条；正文行持续累积，遇其他内容不再延伸
                if let Some(hit) = render.thought_hits.last_mut() {
                    hit.body_end = render.lines.len();
                }
            }
        }
        (Role::Assistant, ContentPart::ToolCall(call)) => {
            let needs_separator = state.prev_reasoning || state.prev_text || prev_web;
            state.prev_reasoning = false;
            state.prev_text = false;
            if needs_separator {
                render.lines.push(Line::from(""));
            }
            push_tool_call(render, call);
        }
        (Role::Tool, ContentPart::ToolResult(_)) => {
            state.prev_reasoning = false;
            state.prev_text = false;
        }
        (Role::Assistant, ContentPart::ImageGenerationCall(call)) => {
            let needs_separator = state.prev_reasoning || state.prev_text;
            state.prev_reasoning = false;
            state.prev_text = false;
            if needs_separator {
                render.lines.push(Line::from(""));
            }
            let mut detail = match call.status {
                ImageGenerationStatus::InProgress => "in_progress",
                ImageGenerationStatus::Completed => "completed",
                ImageGenerationStatus::Incomplete => "incomplete",
            }
            .to_string();
            if let Some(prompt) = &call.revised_prompt {
                detail.push(' ');
                detail.push_str(prompt);
            }
            let spans = vec![
                Span::styled(
                    "image_gen",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(detail, Style::default().fg(Color::Rgb(160, 160, 160))),
            ];
            let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
            render.lines.extend(wrapped);
        }
        (Role::Assistant, ContentPart::WebSearchCall(call)) => {
            let needs_separator = state.prev_reasoning || state.prev_text;
            state.prev_reasoning = false;
            state.prev_text = false;
            let (label, detail) = match &call.action {
                WebSearchAction::Search => (
                    "web_search",
                    format!("{} ({} sources)", call.query, call.sources.len()),
                ),
                WebSearchAction::OpenPage { url } => ("web_open", url.clone()),
                WebSearchAction::FindInPage { url, pattern } => {
                    ("web_find", format!("{url} / {pattern}"))
                }
            };
            if needs_separator {
                render.lines.push(Line::from(""));
            }
            let spans = vec![
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(detail, Style::default().fg(Color::Rgb(160, 160, 160))),
            ];
            let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
            render.lines.extend(wrapped);

            if matches!(call.action, WebSearchAction::OpenPage { .. }) {
                return;
            }

            // Citation titles on the left, domains aligned to the right.
            for (url, title) in citations.iter() {
                if !call_covers_url(call, url) {
                    continue;
                }
                let domain = url_display_domain(url);
                let title = title.trim();
                let title = if title.is_empty() || title == *url {
                    domain
                } else {
                    title
                };
                let domain = truncate(domain, max_width.saturating_sub(4) / 2);
                let right_w = domain.width();
                let title_max = max_width.saturating_sub(3 + right_w + 1);
                let title = truncate(title, title_max);
                let pad = title_max - title.width() + 1;
                render.lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(title, Style::default().fg(Color::Rgb(170, 170, 170))),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(domain.to_string(), Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
        (Role::Assistant, ContentPart::Text { text, annotations }) => {
            state.prev_reasoning = false;
            state.prev_text = true;
            render.lines.push(Line::from(""));
            let (md_lines, md_links) = render_markdown(text, max_width);
            append_md(render, md_lines, md_links, None);
            push_citations_block(render, ctx, annotations, state);
        }
        // WebSearchCall 等未渲染的 part 不打断思考分组的连续性
        _ => {}
    }
}

pub(crate) fn is_empty_session(view: &SessionView) -> bool {
    view.session.turns.iter().all(|t| {
        t.role == Role::System
            || (t.role == Role::User
                && t.messages.iter().all(|m| {
                    m.parts.iter().all(|p| match p {
                        ContentPart::Text { text, .. } => {
                            crate::system::parse_agents_reminder(text).is_some_and(|r| {
                                matches!(
                                    r.kind,
                                    crate::system::AgentsReminderKind::Global
                                        | crate::system::AgentsReminderKind::Project
                                        | crate::system::AgentsReminderKind::Skills
                                )
                            })
                        }
                        _ => false,
                    })
                }))
    }) && view.draft.is_empty()
}

pub(crate) fn push_empty_hint(render: &mut ChatRender<'_>) {
    render.lines.push(Line::from(""));
    render.lines.push(Line::from(vec![
        Span::styled(
            "  Start typing a prompt or use ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            "/",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " to access commands (/sessions to resume).",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
}

pub(crate) fn append_turn_outcome(render: &mut ChatRender<'_>, turn_id: &str) {
    for notice in render.notices {
        if matches!(&notice.kind, crate::harness::NoticeKind::Turn { turn_id: id, .. } if id == turn_id)
        {
            render.lines.push(Line::from(""));
            for text in notice.text.lines() {
                let spans = [Span::styled(
                    text.to_owned(),
                    Style::default().fg(Color::Rgb(220, 120, 120)),
                )];
                render
                    .lines
                    .extend(wrap_spans_to_lines(&spans, render.max_width, "", "   "));
            }
        }
    }
}
