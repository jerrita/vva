use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use super::selection::SelectionTracker;
use crate::model::Session;
use crate::ui::markdown::LinkRegion;
use crate::ui::views::chat::{
    ChatRender, RenderState, append_chat_tail, append_parts, collect_tool_results,
    extract_selected_text,
};

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

/// 历史用户消息在聊天行缓冲中的区间，供点击命中 Revert / Fork
#[derive(Debug, Clone, Copy)]
pub struct UserMessageHit {
    pub turn_index: usize,
    pub line_start: usize,
    pub line_end: usize,
}

impl UserMessageHit {
    pub fn contains(self, line: usize) -> bool {
        line >= self.line_start && line < self.line_end
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
        line >= self.header_start && line < self.header_end
    }

    pub fn contains_block(&self, line: usize) -> bool {
        line >= self.block_start && line < self.block_end
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
        line >= self.line_start && line < self.line_end
    }
}

impl ThoughtHit {
    pub fn contains_header(&self, line: usize) -> bool {
        line >= self.header_start && line < self.header_end
    }

    pub fn contains_body(&self, line: usize) -> bool {
        line >= self.body_start && line < self.body_end
    }
}

/// 仅记录历史前缀边界及其排版状态，不另存文本或持有旧会话快照。
#[derive(Default)]
pub struct ChatCache {
    session: Weak<Session>,
    revision: u64,
    width: usize,
    thinking_concise: bool,
    responses_collapsed: bool,
    notices: usize,
    history_lines: usize,
    render_state: RenderState,
    tool_expand: HashMap<String, ToolExpand>,
    thought_expand: HashMap<String, bool>,
    response_expand: HashMap<String, bool>,
    pub(crate) user_hits: Vec<UserMessageHit>,
    pub(crate) tool_hits: Vec<ToolHit>,
    pub(crate) thought_hits: Vec<ThoughtHit>,
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

    pub fn response_hit_at(&self, line: usize) -> Option<&ResponseHit> {
        self.response_hits.iter().find(|hit| hit.contains(line))
    }
}

impl super::App {
    /// 历史随会话快照和布局失效，草稿与通知随视图版本更新。
    pub fn refresh_chat_lines(&mut self, width: usize) {
        let width = width.max(20);
        let results = collect_tool_results(&self.view);
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
            || self.chat.lines.is_empty()
        {
            self.chat.lines.clear();
            self.chat.links.clear();
            cache.render_state = RenderState::default();
            cache.user_hits.clear();
            cache.tool_hits.clear();
            cache.thought_hits.clear();
            cache.response_hits.clear();
            let turns = &self.view.session.turns;
            let boundaries = std::iter::once(0)
                .chain(turns.iter().enumerate().filter_map(|(index, turn)| {
                    (index > 0
                        && turn.role == crate::model::Role::User
                        && !turn_is_system_reminder(turn))
                    .then_some(index)
                }))
                .chain(std::iter::once(turns.len()))
                .collect::<Vec<_>>();
            for range in boundaries.windows(2) {
                let response_turns = (range[0]..range[1])
                    .filter(|&index| turns[index].role == crate::model::Role::Assistant)
                    .collect::<Vec<_>>();
                let last_assistant = response_turns.last().copied();
                let first_collapsed_assistant = response_turns.first().copied();
                let final_reply = last_assistant.and_then(|index| final_reply_range(&turns[index]));
                let last_part_count = last_assistant
                    .map(|index| {
                        turns[index]
                            .messages
                            .iter()
                            .flat_map(|message| &message.parts)
                            .count()
                    })
                    .unwrap_or(0);
                let response_elapsed_ms = last_assistant.and_then(|index| turns[index].worked_ms);
                let response_group_id = last_assistant.map(|index| turns[index].id.as_str());
                let has_work = final_reply.is_some_and(|(start, end)| {
                    response_turns.len() > 1 || start > 0 || end < last_part_count
                });
                let collapse_response = (range[1] < turns.len() || responses_collapsed)
                    && has_work
                    && response_elapsed_ms.is_some()
                    && response_group_id
                        .is_some_and(|id| !self.response_expand.get(id).copied().unwrap_or(false));
                for (turn_index, turn) in turns.iter().enumerate().take(range[1]).skip(range[0]) {
                    let line_start = self.chat.lines.len();
                    let is_response_turn = turn.role == crate::model::Role::Assistant
                        && response_turns.contains(&turn_index);
                    let is_summary = collapse_response
                        && is_response_turn
                        && Some(turn_index) == first_collapsed_assistant;
                    let collapsible = is_response_turn && has_work && response_elapsed_ms.is_some();
                    let mut render = ChatRender {
                        lines: &mut self.chat.lines,
                        links: &mut self.chat.links,
                        max_width: width,
                        thinking_concise: self.thinking_concise,
                        results: &results,
                        expand: &self.tool_expand,
                        hits: &mut cache.tool_hits,
                        thought_expand: &self.thought_expand,
                        thought_hits: &mut cache.thought_hits,
                        response_hits: &mut cache.response_hits,
                    };
                    if is_summary {
                        crate::ui::views::chat::append_collapsed_response(
                            &mut render,
                            response_group_id.expect("collapsed response has a final turn"),
                            response_elapsed_ms.expect("collapsed response has elapsed time"),
                            &mut cache.render_state,
                        );
                    }
                    if !collapse_response || !is_response_turn {
                        append_parts(
                            &mut render,
                            turn.role,
                            turn.messages.iter().flat_map(|m| &m.parts),
                            &mut cache.render_state,
                        );
                        if turn.role == crate::model::Role::Assistant {
                            cache.response_hits.push(ResponseHit {
                                id: response_group_id
                                    .filter(|_| collapsible)
                                    .unwrap_or(&turn.id)
                                    .to_string(),
                                line_start,
                                line_end: self.chat.lines.len(),
                                collapsed: false,
                                collapsible,
                            });
                        }
                    } else if Some(turn_index) == last_assistant {
                        let parts = turn
                            .messages
                            .iter()
                            .flat_map(|message| &message.parts)
                            .collect::<Vec<_>>();
                        let (start, end) = final_reply.expect("completed response has final text");
                        append_parts(
                            &mut render,
                            turn.role,
                            parts[start..end].iter().copied(),
                            &mut cache.render_state,
                        );
                    }
                    if turn.role == crate::model::Role::User && !turn_is_system_reminder(turn) {
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
        } else if cache.revision == self.view.revision && cache.notices == self.notices.len() {
            return;
        }
        self.chat.lines.truncate(cache.history_lines);
        self.chat
            .links
            .retain(|link| link.line < cache.history_lines);
        cache
            .tool_hits
            .retain(|hit| hit.header_start < cache.history_lines);
        cache
            .thought_hits
            .retain(|hit| hit.header_start < cache.history_lines);
        append_chat_tail(
            &mut ChatRender {
                lines: &mut self.chat.lines,
                links: &mut self.chat.links,
                max_width: width,
                thinking_concise: self.thinking_concise,
                results: &results,
                expand: &self.tool_expand,
                hits: &mut cache.tool_hits,
                thought_expand: &self.thought_expand,
                thought_hits: &mut cache.thought_hits,
                response_hits: &mut cache.response_hits,
            },
            &self.view,
            &self.notices,
            cache.render_state,
        );
        cache.revision = self.view.revision;
        cache.notices = self.notices.len();
    }
}

fn turn_is_system_reminder(turn: &crate::model::Turn) -> bool {
    turn.messages.iter().any(|m| {
        m.parts.iter().any(|p| match p {
            crate::model::ContentPart::Text { text, .. } => {
                crate::system::parse_agents_reminder(text).is_some()
            }
            _ => false,
        })
    })
}

fn final_reply_range(turn: &crate::model::Turn) -> Option<(usize, usize)> {
    let mut last_run = None;
    let mut run_start = None;
    let mut index = 0;
    for part in turn.messages.iter().flat_map(|message| &message.parts) {
        if matches!(part, crate::model::ContentPart::Text { .. }) {
            if run_start.is_none() {
                run_start = Some(index);
            }
        } else if let Some(start) = run_start.take() {
            last_run = Some((start, index));
        }
        index += 1;
    }
    if let Some(start) = run_start {
        last_run = Some((start, index));
    }
    last_run
}
