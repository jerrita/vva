use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

use super::selection::SelectionTracker;
use crate::ui::views::chat::extract_selected_text;

/// 可选中文本面板：渲染行、命中区域与选区是同一件事的三个侧面。
/// 聊天流与侧边栏共用该结构，交互代码按面板列表派发而不逐个枚举目标。
#[derive(Debug, Default)]
pub struct TextPane {
    pub area: Rect,
    pub lines: Vec<Line<'static>>,
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
        self.selection.clear();
    }
}

impl super::App {
    /// 按缓存键重建聊天流渲染行；键未变化时复用上一帧结果
    pub fn refresh_chat_lines(&mut self, width: usize) {
        let key = super::ChatCacheKey {
            revision: self.view.revision,
            width,
            thinking_concise: self.thinking_concise,
            notices: self.notices.len(),
        };
        if key == self.chat_cache_key && !self.chat.lines.is_empty() {
            return;
        }
        self.chat_cache_key = key;
        self.chat.lines = crate::ui::views::chat::build_chat_lines(self, width);
    }
}
