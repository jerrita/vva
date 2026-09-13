use ratatui::layout::Rect;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::watch;

use crate::harness::SessionView;
use crate::harness::{ManagedSession, SessionManager};
use crate::ui::command::SlashCommand;

// 重新导出子模块内容以保持 100% 外部向后兼容
pub use super::history::{SidebarStats, compute_stats};
pub use super::input::InputBuffer;
pub use super::modals::*;
pub use pane::{
    ChatCache, CitationsHit, ResponseHit, TextPane, ThoughtHit, ToolExpand, ToolHit, UserMessageHit,
};
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
    pub scrollbar_area: Rect,
    /// 聊天流面板：渲染与复制共用行缓冲，流式更新只重建历史前缀之后的内容
    pub chat: TextPane,
    pub chat_cache: ChatCache,
    pub sidebar: TextPane,
    pub spinner_frame: usize,
    pub should_quit: bool,
    pub modal: ActiveModal,
    /// 当前弹窗列表区的渲染几何（每帧由弹窗视图回写，供鼠标命中测试）
    pub modal_list_area: Rect,
    pub matched_commands: Vec<&'static SlashCommand>,
    pub selected_command_index: usize,
    pub stats: SidebarStats,
    pub thinking_concise: bool,
    pub tool_expand: HashMap<String, ToolExpand>,
    pub thought_expand: HashMap<String, bool>,
    pub response_expand: HashMap<String, bool>,
    pub citations_expand: HashMap<String, bool>,
    pub last_key_input_time: Option<std::time::Instant>,
    pub input_inner_area: Rect,
    pub model_selector_area: Rect,
    pub variant_selector_area: Rect,
    pub web_toggle_area: Rect,
    pub thinking_toggle_area: Rect,
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
            scrollbar_area: Rect::default(),
            chat: TextPane::default(),
            chat_cache: ChatCache::default(),
            sidebar: TextPane::default(),
            spinner_frame: 0,
            should_quit: false,
            modal: ActiveModal::None,
            modal_list_area: Rect::default(),
            matched_commands: Vec::new(),
            selected_command_index: 0,
            stats,
            thinking_concise: true,
            tool_expand: HashMap::new(),
            thought_expand: HashMap::new(),
            response_expand: HashMap::new(),
            citations_expand: HashMap::new(),
            last_key_input_time: None,
            input_inner_area: Rect::default(),
            model_selector_area: Rect::default(),
            variant_selector_area: Rect::default(),
            web_toggle_area: Rect::default(),
            thinking_toggle_area: Rect::default(),
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
