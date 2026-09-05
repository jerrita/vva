use ratatui::layout::Rect;
use std::sync::Arc;
use tokio::sync::watch;

use crate::harness::SessionView;
use crate::harness::{ManagedSession, SessionManager};
use crate::ui::command::SlashCommand;

// 重新导出子模块内容以保持 100% 外部向后兼容
pub use super::history::{SidebarStats, compute_stats};
pub use super::input::InputBuffer;
pub use super::modals::*;
pub use pane::TextPane;
pub use selection::{
    SelectionTracker, TextCoord, TextSelection, base64_encode, char_in_column_range,
    get_clipboard_text, set_clipboard_text,
};

pub mod dispatch;
pub mod editor;
pub mod pane;
pub mod scroll;
pub mod selection;
pub mod session;

#[cfg(test)]
mod tests;

/// 聊天流行缓存的失效依据：会话视图版本、渲染宽度、思考展示模式与本地通知数量
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChatCacheKey {
    pub revision: u64,
    pub width: usize,
    pub thinking_concise: bool,
    pub notices: usize,
}

/// TUI 交互式应用全局状态与控制中枢
pub struct App {
    pub session_mgr: Arc<SessionManager>,
    pub active_session: Arc<ManagedSession>,
    pub view_rx: watch::Receiver<Arc<SessionView>>,
    pub view: Arc<SessionView>,
    /// 界面通知流：本地提示与会话任务通知按到达顺序汇入同一份
    pub notices: Vec<String>,
    /// 已汇入的会话通知数量
    pub consumed_notices: usize,
    pub input: InputBuffer,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub max_scroll: usize,
    pub viewport_height: usize,
    pub is_dragging_scrollbar: bool,
    /// 聊天流面板：行缓存按 chat_cache_key 失效，渲染与复制读同一份
    pub chat: TextPane,
    pub chat_cache_key: ChatCacheKey,
    pub sidebar: TextPane,
    pub spinner_frame: usize,
    pub should_quit: bool,
    pub modal: ActiveModal,
    pub matched_commands: Vec<&'static SlashCommand>,
    pub selected_command_index: usize,
    pub stats: SidebarStats,
    pub thinking_concise: bool,
    pub last_key_input_time: Option<std::time::Instant>,
    pub input_inner_area: Rect,
    pub prompt_history: Vec<String>,
    pub history_index: Option<usize>,
    pub saved_draft: String,
    pub is_selecting_input: bool,
}

impl App {
    pub async fn new(
        session_mgr: Arc<SessionManager>,
        initial_session: Arc<ManagedSession>,
    ) -> Self {
        let view_rx = initial_session.subscribe();
        let view = initial_session.snapshot();
        let stats = compute_stats(&view.session, session_mgr.app_config());
        Self {
            session_mgr,
            active_session: initial_session,
            view_rx,
            view,
            notices: Vec::new(),
            consumed_notices: 0,
            input: InputBuffer::new(),
            scroll_offset: 0,
            auto_scroll: true,
            max_scroll: 0,
            viewport_height: 0,
            is_dragging_scrollbar: false,
            chat: TextPane::default(),
            chat_cache_key: ChatCacheKey::default(),
            sidebar: TextPane::default(),
            spinner_frame: 0,
            should_quit: false,
            modal: ActiveModal::None,
            matched_commands: Vec::new(),
            selected_command_index: 0,
            stats,
            thinking_concise: false,
            last_key_input_time: None,
            input_inner_area: Rect::default(),
            prompt_history: Vec::new(),
            history_index: None,
            saved_draft: String::new(),
            is_selecting_input: false,
        }
    }

    /// 定时 tick（更新 spinner）
    pub fn tick(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % 4;
    }

    pub fn spinner_char(&self) -> &'static str {
        match self.spinner_frame {
            0 => "⠋",
            1 => "⠙",
            2 => "⠹",
            _ => "⠸",
        }
    }
}
