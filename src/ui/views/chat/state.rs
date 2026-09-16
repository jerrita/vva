//! 渲染状态与输出槽：ChatRender 聚合渲染产物，RenderState 承载跨部件增量状态。

use super::*;

#[derive(Clone, Copy, Default)]
pub(crate) struct RenderState {
    pub(crate) prev_reasoning: bool,
    pub(crate) prev_text: bool,
    pub(crate) prev_web: bool,
    pub(crate) thought_groups: usize,
    pub(crate) thought_expanded: bool,
    pub(crate) thought_inline: bool,
    pub(crate) citation_groups: usize,
}

pub(crate) struct ChatRender<'a> {
    pub lines: &'a mut Vec<Line<'static>>,
    pub links: &'a mut Vec<LinkRegion>,
    pub max_width: usize,
    pub thinking_concise: bool,
    pub results: &'a HashMap<String, &'a ToolResult>,
    pub expand: &'a HashMap<String, ToolExpand>,
    pub hits: &'a mut Vec<ToolHit>,
    pub thought_expand: &'a HashMap<String, bool>,
    pub thought_hits: &'a mut Vec<ThoughtHit>,
    pub citations_expand: &'a HashMap<String, bool>,
    pub citations_hits: &'a mut Vec<CitationsHit>,
    pub response_hits: &'a mut Vec<ResponseHit>,
}

pub(crate) struct PartContext<'a> {
    pub(crate) calls: Vec<&'a WebSearchCall>,
    pub(crate) citations: Vec<(&'a str, &'a str)>,
}

pub(crate) enum ThoughtKind {
    Inline,
    Collapsed,
    Expanded,
}
