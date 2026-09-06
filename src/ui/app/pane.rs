use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::sync::{Arc, Weak};

use super::selection::SelectionTracker;
use crate::model::Session;
use crate::ui::markdown::LinkRegion;
use crate::ui::views::chat::{append_chat_tail, append_parts, extract_selected_text};

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

/// 仅记录历史前缀边界及其排版状态，不另存文本或持有旧会话快照。
#[derive(Default)]
pub struct ChatCache {
    session: Weak<Session>,
    revision: u64,
    width: usize,
    thinking_concise: bool,
    notices: usize,
    history_lines: usize,
    prev_reasoning: bool,
}

impl super::App {
    /// 历史随会话快照和布局失效，草稿与通知随视图版本更新。
    pub fn refresh_chat_lines(&mut self, width: usize) {
        let width = width.max(20);
        let cache = &mut self.chat_cache;
        let session = Arc::downgrade(&self.view.session);
        if !cache.session.ptr_eq(&session)
            || cache.width != width
            || cache.thinking_concise != self.thinking_concise
            || self.chat.lines.is_empty()
        {
            self.chat.lines.clear();
            self.chat.links.clear();
            cache.prev_reasoning = false;
            for turn in &self.view.session.turns {
                append_parts(
                    &mut self.chat.lines,
                    &mut self.chat.links,
                    turn.role,
                    turn.messages.iter().flat_map(|m| &m.parts),
                    width,
                    self.thinking_concise,
                    &mut cache.prev_reasoning,
                );
            }
            cache.history_lines = self.chat.lines.len();
            cache.session = session;
            cache.width = width;
            cache.thinking_concise = self.thinking_concise;
        } else if cache.revision == self.view.revision && cache.notices == self.notices.len() {
            return;
        }
        self.chat.lines.truncate(cache.history_lines);
        self.chat
            .links
            .retain(|link| link.line < cache.history_lines);
        append_chat_tail(
            &mut self.chat.lines,
            &mut self.chat.links,
            &self.view,
            &self.notices,
            width,
            self.thinking_concise,
            cache.prev_reasoning,
        );
        cache.revision = self.view.revision;
        cache.notices = self.notices.len();
    }
}
