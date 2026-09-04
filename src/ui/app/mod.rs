use std::sync::Arc;
use ratatui::layout::Rect;
use ratatui::text::Line;
use tokio::sync::broadcast;

use crate::harness::agent::AgentEvent;
use crate::ui::command::SlashCommand;
use crate::ui::session_manager::{ManagedSession, SessionManager};

// 重新导出子模块内容以保持 100% 外部向后兼容
pub use super::history::{ChatEntry, SidebarStats, compute_stats, entries_from_turns};
pub use super::input::InputBuffer;
pub use super::modals::*;
pub use selection::{TextCoord, TextSelection, base64_encode, get_clipboard_text, set_clipboard_text};

pub mod dispatch;
pub mod editor;
pub mod scroll;
pub mod selection;
pub mod session;

#[cfg(test)]
mod tests;

/// TUI 交互式应用全局状态与控制中枢
pub struct App {
    pub session_mgr: Arc<SessionManager>,
    pub active_session: Arc<ManagedSession>,
    pub event_rx: broadcast::Receiver<AgentEvent>,
    pub chat_entries: Vec<ChatEntry>,
    pub input: InputBuffer,
    pub enable_web_search: bool,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub max_scroll: usize,
    pub viewport_height: usize,
    pub chat_area: Rect,
    pub is_dragging_scrollbar: bool,
    pub text_selection: Option<TextSelection>,
    pub is_selecting_text: bool,
    pub sidebar_area: Rect,
    pub sidebar_lines: Vec<Line<'static>>,
    pub sidebar_selection: Option<TextSelection>,
    pub is_selecting_sidebar: bool,
    pub spinner_frame: usize,
    pub should_quit: bool,
    pub modal: ActiveModal,
    pub sessions_modal: Option<SessionsModalState>,
    pub models_modal: Option<ModelsModalState>,
    pub variants_modal: Option<VariantsModalState>,
    pub config_modal: Option<ConfigModalState>,
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
        enable_web_search: bool,
    ) -> Self {
        let event_rx = initial_session.event_tx.subscribe();
        let effective_web_search = if let Ok(s) = initial_session.session.try_read() {
            s.web_search
        } else {
            enable_web_search
        };
        let mut app = Self {
            session_mgr,
            active_session: initial_session,
            event_rx,
            chat_entries: Vec::new(),
            input: InputBuffer::new(),
            enable_web_search: effective_web_search,
            scroll_offset: 0,
            auto_scroll: true,
            max_scroll: 0,
            viewport_height: 0,
            chat_area: Rect::default(),
            is_dragging_scrollbar: false,
            text_selection: None,
            is_selecting_text: false,
            sidebar_area: Rect::default(),
            sidebar_lines: Vec::new(),
            sidebar_selection: None,
            is_selecting_sidebar: false,
            spinner_frame: 0,
            should_quit: false,
            modal: ActiveModal::None,
            sessions_modal: None,
            models_modal: None,
            variants_modal: None,
            config_modal: None,
            matched_commands: Vec::new(),
            selected_command_index: 0,
            stats: SidebarStats::default(),
            thinking_concise: false,
            last_key_input_time: None,
            input_inner_area: Rect::default(),
            prompt_history: Vec::new(),
            history_index: None,
            saved_draft: String::new(),
            is_selecting_input: false,
        };

        app.reload_active_session_history().await;
        app
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
