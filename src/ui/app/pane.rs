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

/// 工具行展开层级：缺省为折叠一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExpand {
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

/// 仅记录历史前缀边界及其排版状态，不另存文本或持有旧会话快照。
#[derive(Default)]
pub struct ChatCache {
    session: Weak<Session>,
    revision: u64,
    width: usize,
    thinking_concise: bool,
    notices: usize,
    history_lines: usize,
    render_state: RenderState,
    tool_expand: HashMap<String, ToolExpand>,
    pub(crate) user_hits: Vec<UserMessageHit>,
    pub(crate) tool_hits: Vec<ToolHit>,
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
}

impl super::App {
    /// 历史随会话快照和布局失效，草稿与通知随视图版本更新。
    pub fn refresh_chat_lines(&mut self, width: usize) {
        let width = width.max(20);
        let results = collect_tool_results(&self.view);
        let cache = &mut self.chat_cache;
        let session = Arc::downgrade(&self.view.session);
        if !cache.session.ptr_eq(&session)
            || cache.width != width
            || cache.thinking_concise != self.thinking_concise
            || cache.tool_expand != self.tool_expand
            || self.chat.lines.is_empty()
        {
            self.chat.lines.clear();
            self.chat.links.clear();
            cache.render_state = RenderState::default();
            cache.user_hits.clear();
            cache.tool_hits.clear();
            for (turn_index, turn) in self.view.session.turns.iter().enumerate() {
                let line_start = self.chat.lines.len();
                append_parts(
                    &mut ChatRender {
                        lines: &mut self.chat.lines,
                        links: &mut self.chat.links,
                        max_width: width,
                        thinking_concise: self.thinking_concise,
                        results: &results,
                        expand: &self.tool_expand,
                        hits: &mut cache.tool_hits,
                    },
                    turn.role,
                    turn.messages.iter().flat_map(|m| &m.parts),
                    &mut cache.render_state,
                );
                if turn.role == crate::model::Role::User && !turn_is_system_reminder(turn) {
                    cache.user_hits.push(UserMessageHit {
                        turn_index,
                        line_start,
                        line_end: self.chat.lines.len(),
                    });
                }
            }
            cache.history_lines = self.chat.lines.len();
            cache.session = session;
            cache.width = width;
            cache.thinking_concise = self.thinking_concise;
            cache.tool_expand.clone_from(&self.tool_expand);
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
        append_chat_tail(
            &mut ChatRender {
                lines: &mut self.chat.lines,
                links: &mut self.chat.links,
                max_width: width,
                thinking_concise: self.thinking_concise,
                results: &results,
                expand: &self.tool_expand,
                hits: &mut cache.tool_hits,
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
