//! 搜索引用：注脚收集与 Sources 块渲染。

use super::state::*;
use super::*;
use crate::ui::views::{bold, fg};

pub(super) fn url_display_domain(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.split(':').next().unwrap_or(host);
    host.strip_prefix("www.").unwrap_or(host)
}

/// 引用注脚的展示字段（url 与标题）：覆盖 Responses url_citation 与 Anthropic web_search_result_location
pub(super) fn citation_ref(annotation: &Annotation) -> Option<(&str, &str)> {
    match annotation {
        Annotation::UrlCitation { url, title, .. } => Some((url, title)),
        Annotation::WebSearchResultLocation { url, title, .. } => Some((url, title)),
        Annotation::FileCitation { .. } => None,
    }
}

/// Collect cited URLs in first-appearance order within a response.
pub(super) fn search_citations<'a>(
    parts: impl IntoIterator<Item = &'a ContentPart>,
) -> Vec<(&'a str, &'a str)> {
    let mut seen = HashSet::new();
    let mut citations = Vec::new();
    for part in parts {
        if let ContentPart::Text { annotations, .. } = part {
            for a in annotations {
                if let Some((url, title)) = citation_ref(a)
                    && seen.insert(url)
                {
                    citations.push((url, title));
                }
            }
        }
    }
    citations
}

pub(super) fn call_covers_url(call: &WebSearchCall, url: &str) -> bool {
    match call.action.as_ref() {
        Some(WebSearchAction::Search) => call.sources.iter().any(|source| source.url == url),
        Some(WebSearchAction::OpenPage { url: action_url })
        | Some(WebSearchAction::FindInPage {
            url: action_url, ..
        }) => action_url == url,
        None => false,
    }
}

pub(crate) fn push_citations_block(
    render: &mut ChatRender<'_>,
    ctx: &PartContext<'_>,
    annotations: &[crate::model::Annotation],
    state: &mut RenderState,
) {
    let max_width = render.max_width;
    let mut citations: Vec<(&str, &str)> = Vec::new();
    for a in annotations {
        if let Some((url, title)) = citation_ref(a)
            && !ctx.calls.iter().any(|call| call_covers_url(call, url))
        {
            citations.push((title, url));
        }
    }
    // Citations not covered by a call remain below the text, folded by default.
    if citations.is_empty() {
        return;
    }
    render.lines.push(Line::from(""));
    state.citation_groups += 1;
    let id = format!("citations-{}", state.citation_groups);
    let expanded = render.citations_expand.get(&id).copied().unwrap_or(false);
    let marker = if expanded { "-" } else { "+" };
    let header_start = render.lines.len();
    render.lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(
            format!("{marker} Sources / Citations ({})", citations.len()),
            bold(Color::Cyan),
        ),
    ]));
    render.citations_hits.push(CitationsHit {
        id,
        header_start,
        header_end: render.lines.len(),
    });
    if expanded {
        for (i, (title, url)) in citations.iter().enumerate() {
            let spans = vec![
                Span::styled(format!("[{}] ", i + 1), fg(Color::Cyan)),
                Span::styled((*title).to_string(), fg(Color::White)),
                Span::raw(" - "),
                Span::styled((*url).to_string(), fg(Color::Rgb(100, 160, 255))),
            ];
            let wrapped = wrap_spans_to_lines(&spans, max_width, "     ", "       ");
            render.lines.extend(wrapped);
        }
    }
}
