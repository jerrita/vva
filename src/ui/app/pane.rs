use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Weak};

use super::selection::SelectionTracker;
use crate::model::{ContentPart, Role, Session, Turn};
use crate::ui::markdown::LinkRegion;
use crate::ui::views::chat::{
    ChatRender, RenderState, append_collapsed_response, append_parts, append_turn_outcome,
    collect_query_owners, collect_tool_results, extract_selected_text, is_empty_session,
    push_tool_call,
};

use super::draft::DraftRenderCache;

/// 可选中文本面板：渲染行、命中区域与选区是同一件事的三个侧面。
/// 聊天流与侧边栏共用该结构，交互代码按面板列表派发而不逐个枚举目标。
#[derive(Debug, Default)]
pub struct TextPane {
    pub area: Rect,
    pub lines: Vec<Line<'static>>,
    pub links: Vec<LinkRegion>,
    pub selection: SelectionTracker,
}

impl TextPane {
    /// 终端坐标是否落在本面板内
    pub fn hit(&self, column: u16, row: u16) -> bool {
        self.area.width > 0
            && self.area.height > 0
            && self.area.contains(Position { x: column, y: row })
    }

    /// 将终端坐标转换为面板内的相对行列（越界侧按边界收敛）
    pub fn offsets(&self, column: u16, row: u16) -> (usize, usize) {
        let row_offset = row
            .saturating_sub(self.area.y)
            .min(self.area.height.saturating_sub(1)) as usize;
        let col_offset = column
            .saturating_sub(self.area.x)
            .min(self.area.width.saturating_sub(1)) as usize;
        (row_offset, col_offset)
    }

    pub fn selected_text(&self) -> Option<String> {
        extract_selected_text(&self.lines, self.selection.get()?)
    }

    /// 隐藏或切换会话时清空面板
    pub fn reset(&mut self) {
        self.area = Rect::default();
        self.lines.clear();
        self.links.clear();
        self.selection.clear();
    }
}

/// 行号是否落在半开区间 [start, end) 内
fn within(line: usize, start: usize, end: usize) -> bool {
    (start..end).contains(&line)
}

/// 历史用户消息在聊天行缓冲中的区间，供点击命中 Revert / Fork
#[derive(Debug, Clone, Copy)]
pub struct UserMessageHit {
    pub turn_index: usize,
    pub line_start: usize,
    pub line_end: usize,
}

impl UserMessageHit {
    pub fn contains(self, line: usize) -> bool {
        within(line, self.line_start, self.line_end)
    }
}

/// 工具内容块展示层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExpand {
    Hidden,
    Preview,
    Full,
}

/// 工具标题行与展开块在聊天行缓冲中的区间
#[derive(Debug, Clone)]
pub struct ToolHit {
    pub id: String,
    pub header_start: usize,
    pub header_end: usize,
    pub block_start: usize,
    pub block_end: usize,
    pub truncated: bool,
    pub full_only: bool,
}

impl ToolHit {
    pub fn contains_header(&self, line: usize) -> bool {
        within(line, self.header_start, self.header_end)
    }

    pub fn contains_block(&self, line: usize) -> bool {
        within(line, self.block_start, self.block_end)
    }

    pub fn contains(&self, line: usize) -> bool {
        self.contains_header(line) || self.contains_block(line)
    }
}

/// 思考分组标题行与正文在聊天行缓冲中的区间，单击切换该组展开/折叠
#[derive(Debug, Clone)]
pub struct ThoughtHit {
    pub id: String,
    pub header_start: usize,
    pub header_end: usize,
    pub body_start: usize,
    pub body_end: usize,
}

/// 正文末尾引用汇总块的标题行区间，单击切换展开/折叠
#[derive(Debug, Clone)]
pub struct CitationsHit {
    pub id: String,
    pub header_start: usize,
    pub header_end: usize,
}

impl CitationsHit {
    pub fn contains_header(&self, line: usize) -> bool {
        within(line, self.header_start, self.header_end)
    }
}

/// 助手响应在聊天行缓冲中的区间。
#[derive(Debug, Clone)]
pub struct ResponseHit {
    pub id: String,
    pub line_start: usize,
    pub line_end: usize,
    pub collapsed: bool,
    pub collapsible: bool,
}

impl ResponseHit {
    pub fn contains(&self, line: usize) -> bool {
        within(line, self.line_start, self.line_end)
    }
}

impl ThoughtHit {
    pub fn contains_header(&self, line: usize) -> bool {
        within(line, self.header_start, self.header_end)
    }

    pub fn contains_body(&self, line: usize) -> bool {
        within(line, self.body_start, self.body_end)
    }
}

/// 仅记录历史前缀边界及其排版状态，不另存文本或持有旧会话快照。
/// 草稿区由 [`super::draft::DraftRenderCache`] 以部件身份增量渲染。
#[derive(Default)]
pub struct ChatCache {
    session: Weak<Session>,
    revision: u64,
    width: usize,
    thinking_concise: bool,
    responses_collapsed: bool,
    notices: Vec<crate::harness::Notice>,
    shell_outputs: Arc<std::collections::BTreeMap<String, Arc<crate::model::ShellOutput>>>,
    shell_calls: HashMap<String, crate::model::ToolCall>,
    tasks: Arc<std::collections::BTreeMap<String, crate::model::BackgroundTask>>,
    query_owners: HashMap<String, String>,
    history_lines: usize,
    render_state: RenderState,
    tool_expand: HashMap<String, ToolExpand>,
    thought_expand: HashMap<String, bool>,
    response_expand: HashMap<String, bool>,
    citations_expand: HashMap<String, bool>,
    pub(crate) draft: DraftRenderCache,
    pub(crate) user_hits: Vec<UserMessageHit>,
    pub(crate) tool_hits: Vec<ToolHit>,
    pub(crate) thought_hits: Vec<ThoughtHit>,
    pub(crate) citations_hits: Vec<CitationsHit>,
    pub(crate) response_hits: Vec<ResponseHit>,
}

impl ChatCache {
    pub fn user_hit_at(&self, line: usize) -> Option<UserMessageHit> {
        self.user_hits
            .iter()
            .copied()
            .find(|hit| hit.contains(line))
    }

    pub fn tool_hit_at(&self, line: usize) -> Option<&ToolHit> {
        self.tool_hits.iter().find(|hit| hit.contains(line))
    }

    pub fn thought_hit_at(&self, line: usize) -> Option<&ThoughtHit> {
        self.thought_hits
            .iter()
            .find(|hit| hit.contains_header(line) || hit.contains_body(line))
    }

    pub fn citations_hit_at(&self, line: usize) -> Option<&CitationsHit> {
        self.citations_hits
            .iter()
            .find(|hit| hit.contains_header(line))
    }

    pub fn response_hit_at(&self, line: usize) -> Option<&ResponseHit> {
        self.response_hits.iter().find(|hit| hit.contains(line))
    }
}

/// 以 App 的字段切片组装 `ChatRender`（字段级借用，调用方仍可同时持有 cache 等其他字段）
macro_rules! chat_render {
    ($app:ident, $cache:ident, $results:expr, $queries:expr, $width:expr) => {
        ChatRender {
            notices: &$app.view.notices,
            tasks: &$app.view.background_tasks,
            shell_outputs: &$app.view.shell_outputs,
            query_owners: $queries,
            lines: &mut $app.chat.lines,
            links: &mut $app.chat.links,
            max_width: $width,
            thinking_concise: $app.thinking_concise,
            results: $results,
            expand: &$app.tool_expand,
            hits: &mut $cache.tool_hits,
            thought_expand: &$app.thought_expand,
            thought_hits: &mut $cache.thought_hits,
            citations_expand: &$app.citations_expand,
            citations_hits: &mut $cache.citations_hits,
            response_hits: &mut $cache.response_hits,
        }
    };
}

/// 一个运行段（一条真实用户消息到下一条之前）内的助手响应及其折叠判定
struct ResponseGroup<'t> {
    /// 段内全部助手轮次下标
    assistant_turns: Vec<usize>,
    /// 最后一个助手轮次（携带最终答复、耗时与分组 id）
    last: Option<usize>,
    id: Option<&'t str>,
    elapsed_ms: Option<u64>,
    final_reply: Option<(usize, usize)>,
    /// 存在可折叠的过程内容（且已记录耗时）
    collapsible: bool,
    collapse: bool,
}

impl<'t> ResponseGroup<'t> {
    fn new(
        turns: &'t [Turn],
        range: Range<usize>,
        settled: bool,
        response_expand: &HashMap<String, bool>,
    ) -> Self {
        let assistant_turns: Vec<usize> = range
            .clone()
            .filter(|&index| turns[index].role == Role::Assistant)
            .collect();
        let last = assistant_turns.last().copied();
        let final_reply = last.and_then(|index| turns[index].final_reply_range());
        let last_part_count = last.map_or(0, |index| {
            turns[index].messages.iter().flat_map(|m| &m.parts).count()
        });
        let elapsed_ms = last.and_then(|index| turns[index].worked_ms);
        let id = last.map(|index| turns[index].id.as_str());
        let has_work = final_reply.is_some_and(|(start, end)| {
            assistant_turns.len() > 1 || start > 0 || end < last_part_count
        });
        let collapsible = has_work && elapsed_ms.is_some();
        let expanded = id.is_some_and(|id| response_expand.get(id).copied().unwrap_or(false));
        let collapse = (range.end < turns.len() || settled) && collapsible && !expanded;
        Self {
            assistant_turns,
            last,
            id,
            elapsed_ms,
            final_reply,
            collapsible,
            collapse,
        }
    }

    fn is_response_turn(&self, index: usize) -> bool {
        self.assistant_turns.contains(&index)
    }
}

impl super::App {
    /// 历史随会话快照和布局失效，草稿随视图版本更新，关联反馈变化使历史失效。
    pub fn refresh_chat_lines(&mut self, width: usize) {
        let width = width.max(20);
        let results = collect_tool_results(&self.view);
        let query_owners = collect_query_owners(&self.view, &results);
        let responses_collapsed = !self.active_session.is_running();
        let cache = &mut self.chat_cache;
        let session = Arc::downgrade(&self.view.session);
        if !cache.session.ptr_eq(&session)
            || cache.width != width
            || cache.thinking_concise != self.thinking_concise
            || cache.responses_collapsed != responses_collapsed
            || cache.tool_expand != self.tool_expand
            || cache.thought_expand != self.thought_expand
            || cache.response_expand != self.response_expand
            || cache.citations_expand != self.citations_expand
            || cache.notices != self.view.notices
            || !Arc::ptr_eq(&cache.tasks, &self.view.background_tasks)
            || cache.query_owners != query_owners
            || self.chat.lines.is_empty()
        {
            cache.shell_calls = self
                .view
                .session
                .turns
                .iter()
                .flat_map(|turn| &turn.messages)
                .flat_map(|message| &message.parts)
                .filter_map(|part| match part {
                    ContentPart::ToolCall(call) if call.name == crate::tools::names::SHELL => {
                        Some((call.id.clone(), call.clone()))
                    }
                    _ => None,
                })
                .collect();
            self.chat.lines.clear();
            self.chat.links.clear();
            cache.render_state = RenderState::default();
            cache.user_hits.clear();
            cache.tool_hits.clear();
            cache.thought_hits.clear();
            cache.citations_hits.clear();
            cache.response_hits.clear();
            let turns = &self.view.session.turns;
            let boundaries = std::iter::once(0)
                .chain((1..turns.len()).filter(|&index| turn_is_user_boundary(&turns[index])))
                .chain(std::iter::once(turns.len()))
                .collect::<Vec<_>>();
            for range in boundaries.windows(2) {
                let group = ResponseGroup::new(
                    turns,
                    range[0]..range[1],
                    responses_collapsed,
                    &self.response_expand,
                );
                for (turn_index, turn) in turns.iter().enumerate().take(range[1]).skip(range[0]) {
                    let line_start = self.chat.lines.len();
                    let is_response_turn = group.is_response_turn(turn_index);
                    let mut render = chat_render!(self, cache, &results, &query_owners, width);
                    if let (true, Some(id), Some(elapsed_ms)) = (
                        group.collapse
                            && is_response_turn
                            && group.assistant_turns.first() == Some(&turn_index),
                        group.id,
                        group.elapsed_ms,
                    ) {
                        append_collapsed_response(
                            &mut render,
                            id,
                            elapsed_ms,
                            turns[range[0]..range[1]]
                                .iter()
                                .flat_map(|turn| &turn.messages)
                                .flat_map(|message| &message.parts)
                                .filter_map(|part| match part {
                                    ContentPart::ToolResult(result) => {
                                        result.background_task_id.as_ref()
                                    }
                                    _ => None,
                                })
                                .filter(|id| {
                                    self.view.background_tasks.get(*id).is_some_and(|task| {
                                        task.status == crate::model::BackgroundTaskStatus::Running
                                    })
                                })
                                .count(),
                            &mut cache.render_state,
                        );
                    }
                    if !group.collapse || !is_response_turn {
                        append_parts(
                            &mut render,
                            turn.role,
                            turn.messages.iter().flat_map(|m| &m.parts),
                            &mut cache.render_state,
                        );
                        if turn.role == Role::Assistant {
                            render.response_hits.push(ResponseHit {
                                id: group
                                    .id
                                    .filter(|_| group.collapsible && is_response_turn)
                                    .unwrap_or(&turn.id)
                                    .to_string(),
                                line_start,
                                line_end: render.lines.len(),
                                collapsed: false,
                                collapsible: is_response_turn && group.collapsible,
                            });
                        }
                    } else if Some(turn_index) == group.last {
                        let parts = turn
                            .messages
                            .iter()
                            .flat_map(|message| &message.parts)
                            .collect::<Vec<_>>();
                        let (start, end) = group
                            .final_reply
                            .expect("completed response has final text");
                        append_parts(
                            &mut render,
                            turn.role,
                            parts[start..end].iter().copied(),
                            &mut cache.render_state,
                        );
                    }
                    append_turn_outcome(&mut render, &turn.id);
                    if turn.role == Role::User && !turn_is_system_reminder(turn) {
                        cache.user_hits.push(UserMessageHit {
                            turn_index,
                            line_start,
                            line_end: self.chat.lines.len(),
                        });
                    }
                }
            }
            cache.history_lines = self.chat.lines.len();
            cache.session = session;
            cache.width = width;
            cache.thinking_concise = self.thinking_concise;
            cache.responses_collapsed = responses_collapsed;
            cache.tool_expand.clone_from(&self.tool_expand);
            cache.thought_expand.clone_from(&self.thought_expand);
            cache.response_expand.clone_from(&self.response_expand);
            cache.citations_expand.clone_from(&self.citations_expand);
            cache.draft.reset();
        } else {
            if cache.revision == self.view.revision {
                return;
            }
            if !Arc::ptr_eq(&cache.shell_outputs, &self.view.shell_outputs) {
                // 只重排变化的 Shell 块，保留历史文本、Markdown 与其他工具的已渲染行。
                // 从后向前替换，使尚未处理的块坐标保持有效。
                let changed: Vec<_> = cache
                    .tool_hits
                    .iter()
                    .filter(|hit| {
                        match (
                            cache.shell_outputs.get(&hit.id),
                            self.view.shell_outputs.get(&hit.id),
                        ) {
                            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => false,
                            (Some(a), Some(b))
                                if self.tool_expand.get(&hit.id) == Some(&ToolExpand::Hidden) =>
                            {
                                a.finished != b.finished || a.error != b.error
                            }
                            (None, None) => false,
                            _ => true,
                        }
                    })
                    .cloned()
                    .collect();
                let mut shifted = false;
                for old in changed.into_iter().rev() {
                    let Some(call) = cache.shell_calls.get(&old.id).cloned() else {
                        continue;
                    };
                    let mut lines = Vec::new();
                    let mut hits = Vec::new();
                    let mut links = Vec::new();
                    {
                        let mut render = chat_render!(self, cache, &results, &query_owners, width);
                        render.lines = &mut lines;
                        render.hits = &mut hits;
                        render.links = &mut links;
                        push_tool_call(&mut render, &call);
                    }
                    let end = if old.block_end > old.header_end {
                        old.block_end + 1
                    } else {
                        old.header_end
                    };
                    let change = lines.len() as isize - (end - old.header_start) as isize;
                    shifted |= change != 0;
                    self.chat.lines.splice(old.header_start..end, lines);
                    cache.shift_lines(end, change);
                    for link in &mut self.chat.links {
                        shift_line(&mut link.line, end, change);
                    }
                    if !self.auto_scroll {
                        shift_line(&mut self.scroll_offset, end, change);
                    }
                    let mut hit = hits.pop().expect("shell tool hit");
                    hit.header_start += old.header_start;
                    hit.header_end += old.header_start;
                    hit.block_start += old.header_start;
                    hit.block_end += old.header_start;
                    if let Some(previous) = cache.tool_hits.iter_mut().find(|hit| hit.id == old.id)
                    {
                        *previous = hit;
                    }
                }
                // 草稿边界可能移动，重置其行坐标；历史内容保持缓存。
                if shifted {
                    cache.draft.reset();
                }
            }
        }
        // 草稿区：历史边界之后增量渲染，未变部件与已提交行不重算
        let draft = self.view.draft.clone();
        let empty_session = is_empty_session(&self.view);
        let history_lines = cache.history_lines;
        let mut state = cache.render_state;
        {
            let mut render = chat_render!(self, cache, &results, &query_owners, width);
            cache.draft.render(
                &mut render,
                &draft,
                history_lines,
                &mut state,
                empty_session,
            );
            if state.prev_text {
                render.lines.push(Line::from(""));
            }
        }
        cache.revision = self.view.revision;
        cache.notices.clone_from(&self.view.notices);
        cache.tasks.clone_from(&self.view.background_tasks);
        cache.shell_outputs.clone_from(&self.view.shell_outputs);
        cache.query_owners = query_owners;
    }
}

fn turn_is_system_reminder(turn: &Turn) -> bool {
    turn.messages.iter().flat_map(|m| &m.parts).any(|part| {
        matches!(part, ContentPart::Text { text, .. } if crate::system::parse_agents_reminder(text).is_some())
    })
}

/// 运行段边界：真实用户消息；机器注入 reminder（含后台任务通知）随行注入不算
fn turn_is_user_boundary(turn: &Turn) -> bool {
    turn.role == Role::User && !turn_is_system_reminder(turn)
}

fn shift_line(line: &mut usize, from: usize, change: isize) {
    if *line >= from {
        *line = line.saturating_add_signed(change);
    }
}

impl ChatCache {
    fn shift_lines(&mut self, from: usize, change: isize) {
        shift_line(&mut self.history_lines, from, change);
        for hit in &mut self.tool_hits {
            shift_line(&mut hit.header_start, from, change);
            shift_line(&mut hit.header_end, from, change);
            shift_line(&mut hit.block_start, from, change);
            shift_line(&mut hit.block_end, from, change);
        }
        for hit in &mut self.thought_hits {
            shift_line(&mut hit.header_start, from, change);
            shift_line(&mut hit.header_end, from, change);
            shift_line(&mut hit.body_start, from, change);
            shift_line(&mut hit.body_end, from, change);
        }
        for hit in &mut self.citations_hits {
            shift_line(&mut hit.header_start, from, change);
            shift_line(&mut hit.header_end, from, change);
        }
        for hit in &mut self.user_hits {
            shift_line(&mut hit.line_start, from, change);
            shift_line(&mut hit.line_end, from, change);
        }
        for hit in &mut self.response_hits {
            shift_line(&mut hit.line_start, from, change);
            shift_line(&mut hit.line_end, from, change);
        }
    }
}
