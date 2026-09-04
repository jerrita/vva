use std::sync::Arc;
use ratatui::layout::Rect;
use tokio::sync::broadcast;

use crate::harness::agent::AgentEvent;
use crate::ui::command::{SlashCommand, match_commands};
use crate::ui::session_manager::{ManagedSession, SessionManager};

// 重新导出子模块内容以保持 100% 外部向后兼容
pub use super::history::{ChatEntry, SidebarStats, compute_stats, entries_from_turns};
pub use super::input::InputBuffer;
pub use super::modals::*;

/// 对话流中的文本绝对行列坐标
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCoord {
    pub line: usize,
    pub col: usize,
}

/// 鼠标拖拽文本选区
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: TextCoord,
    pub head: TextCoord,
}

impl TextSelection {
    pub fn new(anchor: TextCoord, head: TextCoord) -> Self {
        Self { anchor, head }
    }

    /// 返回规范化后的起止坐标 (start, end)，满足 start <= end
    pub fn normalized(&self) -> (TextCoord, TextCoord) {
        if self.anchor.line < self.head.line
            || (self.anchor.line == self.head.line && self.anchor.col <= self.head.col)
        {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// 判断选区是否为空（起止点重合）
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

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

    /// 重新加载当前活跃会话的对话历史和统计指标
    pub async fn reload_active_session_history(&mut self) {
        let (turns, session_clone) = {
            let s = self.active_session.session.read().await;
            (s.turns.clone(), s.clone())
        };
        self.chat_entries = entries_from_turns(&turns);
        self.stats = compute_stats(&session_clone, self.session_mgr.app_config());
        self.scroll_offset = 0;
        self.max_scroll = 0;
        self.auto_scroll = true;
    }

    /// 切换当前活跃 Session（单例保持，不干扰原 session 后台工作）
    pub async fn switch_to_session(&mut self, session_id: &str) -> Result<(), String> {
        let target = self.session_mgr.get_or_load(session_id).await?;
        self.active_session = target;
        self.event_rx = self.active_session.event_tx.subscribe();
        self.clear_text_selection();

        let session_web_search = {
            let s = self.active_session.session.read().await;
            s.web_search
        };
        self.enable_web_search = session_web_search;
        if let Some(ref mut cm) = self.config_modal {
            cm.update_web_search(session_web_search);
        }

        self.reload_active_session_history().await;
        self.chat_entries.push(ChatEntry::SystemNotice {
            text: format!("Resumed session {}", session_id),
        });
        Ok(())
    }

    /// 打开 /sessions 模态框并获取最新列表
    pub async fn open_sessions_modal(&mut self) {
        if let Ok(all_sessions) = self.session_mgr.store().list_sessions().await {
            let state = SessionsModalState::new(all_sessions, self.session_mgr.workspace());
            self.sessions_modal = Some(state);
            self.modal = ActiveModal::Sessions;
        } else {
            self.chat_entries.push(ChatEntry::SystemNotice {
                text: "Failed to list sessions from store".into(),
            });
        }
    }

    /// 获取当前活跃 Session 的模型名称
    pub fn current_session_model(&self) -> String {
        if let Ok(guard) = self.active_session.session.try_read() {
            guard.current_model.clone()
        } else {
            String::new()
        }
    }

    /// 打开 /model 模态框并获取当前配置的所有可用模型
    pub async fn open_models_modal(&mut self) {
        let current_model = {
            let s = self.active_session.session.read().await;
            s.current_model.clone()
        };

        let mut models = Vec::new();
        if let Some(cfg) = self.session_mgr.app_config() {
            for (p_id, p_cfg) in &cfg.providers {
                for (m_id, m_cfg) in &p_cfg.models {
                    let effective_type = m_cfg.effective_type(p_cfg.provider_type);
                    models.push(ModelItem {
                        full_id: format!("{p_id}/{m_id}"),
                        provider_id: p_id.clone(),
                        model_id: m_id.clone(),
                        display_name: m_cfg.name.clone(),
                        provider_type: format!("{:?}", effective_type).to_lowercase(),
                        reasoning: m_cfg.reasoning,
                        context_limit: m_cfg.limit.as_ref().map(|l| l.context),
                        variants: m_cfg.variants.clone(),
                    });
                }
            }
        }
        models.sort_by(|a, b| a.full_id.cmp(&b.full_id));

        self.models_modal = Some(ModelsModalState::new(models, &current_model));
        self.modal = ActiveModal::Models;
    }

    /// 切换模型并持久化保存 Session Header
    pub async fn switch_model(&mut self, model_spec: &str) {
        let (sid, current_var) = {
            let s = self.active_session.session.read().await;
            (s.id.clone(), s.current_variant.clone())
        };

        // 校验新模型是否支持当前 variant，不支持则回退到新模型的默认档位
        let new_variant = if let Some(cfg) = self.session_mgr.app_config() {
            if let Ok(resolved) = cfg.resolve_model(model_spec, current_var.as_deref()) {
                resolved.selected_variant
            } else if let Ok(fallback) = cfg.resolve_model(model_spec, None) {
                fallback.selected_variant
            } else {
                None
            }
        } else {
            current_var
        };

        {
            let mut s = self.active_session.session.write().await;
            s.set_model(model_spec.to_string(), new_variant);
        }

        let s_clone = {
            let s_ref = self.active_session.session.read().await;
            if self.active_session.is_persisted() {
                let _ = self.session_mgr.store().save_header(&s_ref).await;
            }
            s_ref.clone()
        };

        self.chat_entries.push(ChatEntry::SystemNotice {
            text: format!("Switched model to '{model_spec}' for session {sid}"),
        });
        self.stats = compute_stats(&s_clone, self.session_mgr.app_config());
    }

    /// 打开 /variant 模态框并基于当前模型获取可用的 reasoning effort 档位
    pub async fn open_variants_modal(&mut self) {
        let (current_model, current_variant) = {
            let s = self.active_session.session.read().await;
            (s.current_model.clone(), s.current_variant.clone())
        };

        // 获取当前模型在配置中的 ModelConfig（若存在）
        let model_cfg = if let Some(cfg) = self.session_mgr.app_config() {
            if let Ok(resolved) = cfg.resolve_model(&current_model, None) {
                Some(resolved.model_config)
            } else {
                let mut found = None;
                for p_cfg in cfg.providers.values() {
                    if let Some(m) = p_cfg.models.get(&current_model) {
                        found = Some(m.clone());
                        break;
                    }
                }
                found
            }
        } else {
            None
        };

        let mut items = Vec::new();
        // 1. None / Default 选项
        items.push(VariantItem {
            id: None,
            display_name: "none (default)".to_string(),
            description: "Default reasoning effort or disable reasoning override".to_string(),
            is_default: true,
        });

        // 2. 根据模型配置构建候选档位
        if let Some(cfg) = &model_cfg {
            if let Some(valid_vars) = &cfg.variants {
                for v in valid_vars {
                    let desc = match v.as_str() {
                        "minimal" => "Minimal reasoning effort (fastest response, light thinking)",
                        "low" => "Low reasoning effort (quick thinking for simpler tasks)",
                        "medium" => "Medium reasoning effort (balanced reasoning and speed)",
                        "high" => "High reasoning effort (deep thinking for complex coding tasks)",
                        "xhigh" => "Extra high reasoning effort (extended exploration)",
                        "max" => "Maximum reasoning effort (exhaustive verification)",
                        _ => "Configured model reasoning variant",
                    };
                    items.push(VariantItem {
                        id: Some(v.clone()),
                        display_name: v.clone(),
                        description: desc.to_string(),
                        is_default: false,
                    });
                }
            } else {
                let standard_tiers = [
                    (
                        "minimal",
                        "Minimal reasoning effort (fastest response, light thinking)",
                    ),
                    (
                        "low",
                        "Low reasoning effort (quick thinking for simpler tasks)",
                    ),
                    (
                        "medium",
                        "Medium reasoning effort (balanced reasoning and speed)",
                    ),
                    (
                        "high",
                        "High reasoning effort (deep thinking for complex coding tasks)",
                    ),
                    (
                        "xhigh",
                        "Extra high reasoning effort (extended exploration)",
                    ),
                    ("max", "Maximum reasoning effort (exhaustive verification)"),
                ];
                for (v, desc) in standard_tiers {
                    items.push(VariantItem {
                        id: Some(v.to_string()),
                        display_name: v.to_string(),
                        description: desc.to_string(),
                        is_default: false,
                    });
                }
            }
        } else {
            let standard_tiers = [
                (
                    "minimal",
                    "Minimal reasoning effort (fastest response, light thinking)",
                ),
                (
                    "low",
                    "Low reasoning effort (quick thinking for simpler tasks)",
                ),
                (
                    "medium",
                    "Medium reasoning effort (balanced reasoning and speed)",
                ),
                (
                    "high",
                    "High reasoning effort (deep thinking for complex coding tasks)",
                ),
                (
                    "xhigh",
                    "Extra high reasoning effort (extended exploration)",
                ),
                ("max", "Maximum reasoning effort (exhaustive verification)"),
            ];
            for (v, desc) in standard_tiers {
                items.push(VariantItem {
                    id: Some(v.to_string()),
                    display_name: v.to_string(),
                    description: desc.to_string(),
                    is_default: false,
                });
            }
        }

        // 如果当前生效的 variant 不在列表中，追加展示为自定义项
        if let Some(ref cur) = current_variant
            && !items
                .iter()
                .any(|it| it.id.as_deref() == Some(cur.as_str()))
        {
            items.push(VariantItem {
                id: Some(cur.clone()),
                display_name: cur.clone(),
                description: "Currently active custom variant".to_string(),
                is_default: false,
            });
        }

        self.variants_modal = Some(VariantsModalState::new(
            current_model,
            current_variant,
            items,
        ));
        self.modal = ActiveModal::Variants;
    }

    /// 切换推理强度档位并持久化保存 Session Header
    pub async fn switch_variant(&mut self, variant: Option<&str>) {
        let (sid, current_model) = {
            let s = self.active_session.session.read().await;
            (s.id.clone(), s.current_model.clone())
        };

        let new_variant = match variant {
            Some(v) if v == "none" || v == "default" || v.is_empty() => None,
            Some(v) => Some(v.to_string()),
            None => None,
        };

        if let (Some(v), Some(cfg)) = (&new_variant, self.session_mgr.app_config())
            && let Err(e) = cfg.resolve_model(&current_model, Some(v))
        {
            self.chat_entries.push(ChatEntry::SystemNotice {
                text: format!("Notice: {e}"),
            });
        }

        {
            let mut s = self.active_session.session.write().await;
            s.set_model(current_model, new_variant.clone());
        }

        let s_clone = {
            let s_ref = self.active_session.session.read().await;
            if self.active_session.is_persisted() {
                let _ = self.session_mgr.store().save_header(&s_ref).await;
            }
            s_ref.clone()
        };

        let display = new_variant.as_deref().unwrap_or("none");
        self.chat_entries.push(ChatEntry::SystemNotice {
            text: format!("Set reasoning variant to '{display}' for session {sid}"),
        });
        self.stats = compute_stats(&s_clone, self.session_mgr.app_config());
    }

    /// 打开 /config 模态框，交互式查看与调整整体会话配置
    pub async fn open_config_modal(&mut self) {
        let (current_model, current_variant) = {
            let s = self.active_session.session.read().await;
            (s.current_model.clone(), s.current_variant.clone())
        };

        let state = ConfigModalState::new(
            current_model,
            current_variant,
            self.enable_web_search,
            self.thinking_concise,
        );
        self.config_modal = Some(state);
        self.modal = ActiveModal::Config;
    }

    // --- 滚动流控制方法 ---

    /// 更新当前对话流的可视区与最大可滚动行数，自适应维持滚动位置
    pub fn update_viewport(&mut self, max_scroll: usize, viewport_height: usize) {
        self.max_scroll = max_scroll;
        self.viewport_height = viewport_height;
        if self.auto_scroll {
            self.scroll_offset = max_scroll;
        } else {
            self.scroll_offset = self.scroll_offset.min(max_scroll);
        }
    }

    /// 向上滚动（查看更早历史消息）
    pub fn scroll_up(&mut self, delta: usize) {
        if self.max_scroll == 0 {
            return;
        }
        if self.auto_scroll {
            self.auto_scroll = false;
            self.scroll_offset = self.max_scroll.saturating_sub(delta);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(delta);
        }
    }

    /// 向下滚动（接近最新消息），若到达底部则恢复自动吸底模式
    pub fn scroll_down(&mut self, delta: usize) {
        if self.max_scroll == 0 {
            return;
        }
        if !self.auto_scroll {
            let next_offset = self.scroll_offset.saturating_add(delta);
            if next_offset >= self.max_scroll {
                self.scroll_offset = self.max_scroll;
                self.auto_scroll = true;
            } else {
                self.scroll_offset = next_offset;
            }
        }
    }

    /// 一键滚动到会话顶部
    pub fn scroll_to_top(&mut self) {
        if self.max_scroll > 0 {
            self.auto_scroll = false;
            self.scroll_offset = 0;
        }
    }

    /// 一键滚动到底部并开启自动吸底
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll;
        self.auto_scroll = true;
    }

    /// 根据比例 (0.0 ..= 1.0) 精确设置滚动条与视口偏移量（用于滚动条点击或平滑拖拽）
    pub fn scroll_to_ratio(&mut self, ratio: f64) {
        if self.max_scroll == 0 {
            return;
        }
        let clamped = ratio.clamp(0.0, 1.0);
        let target = (clamped * self.max_scroll as f64).round() as usize;
        if target >= self.max_scroll {
            self.scroll_offset = self.max_scroll;
            self.auto_scroll = true;
        } else {
            self.scroll_offset = target;
            self.auto_scroll = false;
        }
    }

    // --- 文本选区与剪贴板控制方法 ---

    pub fn start_text_selection(&mut self, line: usize, col: usize) {
        let coord = TextCoord { line, col };
        self.text_selection = Some(TextSelection::new(coord, coord));
        self.is_selecting_text = true;
        self.auto_scroll = false;
    }

    pub fn update_text_selection(&mut self, line: usize, col: usize) {
        if let Some(ref mut sel) = self.text_selection {
            sel.head = TextCoord { line, col };
        }
    }

    pub fn finish_text_selection(&mut self) {
        self.is_selecting_text = false;
        if let Some(ref sel) = self.text_selection
            && sel.is_empty()
        {
            self.text_selection = None;
        }
    }

    pub fn clear_text_selection(&mut self) {
        self.text_selection = None;
        self.is_selecting_text = false;
    }

    pub fn copy_selection_to_clipboard(&mut self) -> bool {
        if let Some(text) = self.input.selected_text()
            && !text.is_empty()
        {
            let _ = set_clipboard_text(&text);
            return true;
        }
        let max_width = (self.chat_area.width as usize).saturating_sub(1).max(20);
        let lines = crate::ui::views::chat::build_chat_lines(self, max_width);
        if let Some(ref sel) = self.text_selection
            && let Some(text) = crate::ui::views::chat::extract_selected_text(&lines, sel)
            && !text.is_empty()
        {
            let _ = set_clipboard_text(&text);
            return true;
        }
        false
    }

    // --- 输入框代理方法 ---

    pub fn insert_char(&mut self, c: char) {
        self.history_index = None;
        self.input.insert_char(c);
        self.last_key_input_time = Some(std::time::Instant::now());
        self.update_command_matches();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.history_index = None;
        self.input.insert_str(s);
        self.last_key_input_time = Some(std::time::Instant::now());
        self.update_command_matches();
    }

    /// 从系统剪贴板粘贴文本到当前输入框 (保留多行格式，不提前提交)
    pub fn paste_from_clipboard(&mut self) -> bool {
        if let Some(text) = get_clipboard_text() {
            self.insert_str(&text);
            true
        } else {
            false
        }
    }

    pub fn backspace(&mut self) {
        self.history_index = None;
        self.input.backspace();
        self.update_command_matches();
    }

    pub fn delete(&mut self) {
        self.history_index = None;
        self.input.delete();
        self.update_command_matches();
    }

    pub fn set_cursor_from_mouse(&mut self, target_row: usize, target_visual_col: usize) -> usize {
        self.input.set_cursor_from_visual_coord(target_row, target_visual_col)
    }

    pub fn start_input_selection(&mut self, char_idx: usize) {
        self.input.selection = Some((char_idx, char_idx));
        self.is_selecting_input = true;
    }

    pub fn update_input_selection(&mut self, char_idx: usize) {
        if let Some((anchor, _)) = self.input.selection {
            self.input.selection = Some((anchor, char_idx));
        } else {
            self.input.selection = Some((char_idx, char_idx));
        }
    }

    pub fn finish_input_selection(&mut self) {
        self.is_selecting_input = false;
        if let Some((anchor, head)) = self.input.selection
            && anchor == head
        {
            self.input.selection = None;
        }
    }

    pub fn clear_input_selection(&mut self) {
        self.is_selecting_input = false;
        self.input.clear_selection();
    }

    /// 向上轮换历史 Prompt
    pub fn history_previous(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }

        match self.history_index {
            None => {
                self.saved_draft = self.input.text.clone();
                let last_idx = self.prompt_history.len() - 1;
                self.history_index = Some(last_idx);
                let text = self.prompt_history[last_idx].clone();
                self.set_input(&text);
            }
            Some(idx) if idx > 0 => {
                let prev_idx = idx - 1;
                self.history_index = Some(prev_idx);
                let text = self.prompt_history[prev_idx].clone();
                self.set_input(&text);
            }
            Some(_) => {}
        }
    }

    /// 向下轮换历史 Prompt
    pub fn history_next(&mut self) {
        if let Some(idx) = self.history_index {
            if idx + 1 < self.prompt_history.len() {
                let next_idx = idx + 1;
                self.history_index = Some(next_idx);
                let text = self.prompt_history[next_idx].clone();
                self.set_input(&text);
            } else {
                self.history_index = None;
                let draft = std::mem::take(&mut self.saved_draft);
                self.set_input(&draft);
            }
        }
    }

    pub fn move_cursor_left(&mut self) {
        self.input.move_cursor_left();
    }

    pub fn move_cursor_right(&mut self) {
        self.input.move_cursor_right();
    }

    pub fn move_cursor_up(&mut self) {
        self.input.move_cursor_up();
    }

    pub fn move_cursor_down(&mut self) {
        self.input.move_cursor_down();
    }

    pub fn move_cursor_home(&mut self) {
        self.input.move_cursor_home();
    }

    pub fn move_cursor_end(&mut self) {
        self.input.move_cursor_end();
    }

    pub fn input_line_count(&self) -> usize {
        self.input.line_count()
    }

    pub fn cursor_line_and_col(&self) -> (usize, usize) {
        self.input.cursor_line_and_col()
    }

    pub fn set_input(&mut self, s: &str) {
        self.input.set(s);
        self.update_command_matches();
    }

    pub fn clear_input(&mut self) {
        self.clear_input_selection();
        self.input.clear();
        self.matched_commands.clear();
        self.selected_command_index = 0;
    }

    // --- 斜杠命令匹配与自动补全 ---

    pub fn update_command_matches(&mut self) {
        if self.input.trim_start().starts_with('/') {
            self.matched_commands = match_commands(&self.input);
            if self.selected_command_index >= self.matched_commands.len() {
                self.selected_command_index = 0;
            }
        } else {
            self.matched_commands.clear();
            self.selected_command_index = 0;
        }
    }

    pub fn command_select_previous(&mut self) {
        if !self.matched_commands.is_empty() {
            self.selected_command_index = if self.selected_command_index == 0 {
                self.matched_commands.len() - 1
            } else {
                self.selected_command_index - 1
            };
        }
    }

    pub fn command_select_next(&mut self) {
        if !self.matched_commands.is_empty() {
            self.selected_command_index =
                (self.selected_command_index + 1) % self.matched_commands.len();
        }
    }

    pub fn complete_selected_command(&mut self) {
        if let Some(cmd) = self.matched_commands.get(self.selected_command_index) {
            self.input.set(&format!("{} ", cmd.name));
            self.update_command_matches();
        }
    }

    // --- 输入提交与斜杠命令执行 ---

    pub async fn submit_input(&mut self) {
        let input_text = self.input.trim().to_string();
        if input_text.is_empty() {
            return;
        }

        // 记录非重复历史 Prompt
        if self.prompt_history.last() != Some(&input_text) {
            self.prompt_history.push(input_text.clone());
        }
        self.history_index = None;
        self.saved_draft.clear();

        self.clear_input();
        self.clear_text_selection();

        // 处理斜杠命令
        if input_text.starts_with('/') {
            self.handle_slash_command(&input_text).await;
            return;
        }

        // 普通用户 Prompt
        self.chat_entries.push(ChatEntry::UserPrompt {
            text: input_text.clone(),
        });
        self.auto_scroll = true;

        if let Err(e) = self
            .session_mgr
            .submit_prompt(
                self.active_session.clone(),
                &input_text,
            )
            .await
        {
            self.chat_entries.push(ChatEntry::SystemNotice {
                text: format!("Error: {e}"),
            });
        }
    }

    /// 处理斜杠命令分发
    async fn handle_slash_command(&mut self, cmd_line: &str) {
        let mut parts = cmd_line.split_whitespace();
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.collect::<Vec<&str>>().join(" ");

        match cmd.as_str() {
            "/sessions" | "/session" => {
                self.open_sessions_modal().await;
            }
            "/model" | "/m" => {
                if arg.is_empty() {
                    self.open_models_modal().await;
                } else {
                    self.switch_model(&arg).await;
                }
            }
            "/variant" | "/v" | "/effort" => {
                if arg.is_empty() {
                    self.open_variants_modal().await;
                } else {
                    self.switch_variant(Some(&arg)).await;
                }
            }
            "/config" | "/settings" => {
                self.open_config_modal().await;
            }
            "/search" | "/web" => {
                if arg.eq_ignore_ascii_case("on") {
                    self.enable_web_search = true;
                } else if arg.eq_ignore_ascii_case("off") {
                    self.enable_web_search = false;
                } else {
                    self.enable_web_search = !self.enable_web_search;
                }
                {
                    let mut s = self.active_session.session.write().await;
                    s.set_web_search(self.enable_web_search);
                }
                if self.active_session.is_persisted() {
                    let s_ref = self.active_session.session.read().await;
                    let _ = self.session_mgr.store().save_header(&s_ref).await;
                }

                let status = if self.enable_web_search { "ON" } else { "OFF" };
                if let Some(ref mut cm) = self.config_modal {
                    cm.update_web_search(self.enable_web_search);
                }
                self.chat_entries.push(ChatEntry::SystemNotice {
                    text: format!("Native web search is now {status}"),
                });
            }
            "/thinking" | "/think" | "/th" => {
                self.thinking_concise = !self.thinking_concise;
                let mode_str = if self.thinking_concise {
                    "concise (1 line)"
                } else {
                    "full"
                };
                if let Some(ref mut cm) = self.config_modal {
                    cm.update_thinking_display(self.thinking_concise);
                }
                self.chat_entries.push(ChatEntry::SystemNotice {
                    text: format!("Thinking display mode set to {mode_str}."),
                });
            }
            "/clear" | "/c" => {
                self.chat_entries.clear();
                self.scroll_offset = 0;
                self.max_scroll = 0;
                self.auto_scroll = true;
            }
            "/help" | "/h" | "/?" => {
                self.modal = ActiveModal::Help;
            }
            "/quit" | "/exit" | "/q" => {
                self.should_quit = true;
            }
            _ => {
                self.chat_entries.push(ChatEntry::SystemNotice {
                    text: format!("Unknown command '{cmd}'. Type /help for available commands."),
                });
            }
        }
    }

    /// 处理后台接收到的 AgentEvent
    pub async fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Thought(thought) => {
                if let Some(ChatEntry::Thinking { content }) = self.chat_entries.last_mut() {
                    content.push_str(&thought);
                } else {
                    self.chat_entries
                        .push(ChatEntry::Thinking { content: thought });
                }
            }
            AgentEvent::ToolExecuting { name, args } => {
                self.chat_entries.push(ChatEntry::ToolCall { name, args });
            }
            AgentEvent::ToolFinished {
                name,
                result,
                is_error,
            } => {
                self.chat_entries.push(ChatEntry::ToolResult {
                    name,
                    result,
                    is_error,
                });
            }
            AgentEvent::Text(text) => {
                if let Some(ChatEntry::AssistantMessage { text: existing, .. }) =
                    self.chat_entries.last_mut()
                {
                    existing.push_str(&text);
                } else {
                    self.chat_entries.push(ChatEntry::AssistantMessage {
                        text,
                        citations: Vec::new(),
                    });
                }
            }
            AgentEvent::Citation { title, url } => {
                if let Some(ChatEntry::AssistantMessage { citations, .. }) =
                    self.chat_entries.last_mut()
                    && !citations.iter().any(|(t, u)| t == &title && u == &url)
                {
                    citations.push((title, url));
                }
            }
            AgentEvent::Finished => {
                let s_clone = {
                    let s = self.active_session.session.read().await;
                    s.clone()
                };
                self.stats = compute_stats(&s_clone, self.session_mgr.app_config());
            }
            AgentEvent::Error(err_msg) => {
                self.chat_entries
                    .push(ChatEntry::SystemNotice { text: err_msg });
            }
        }
        if self.auto_scroll {
            self.scroll_offset = self.max_scroll;
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

/// 将文本设置到系统剪贴板（支持原生系统剪贴板与 ANSI OSC 52 转义序列）
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    let mut success = false;
    if let Ok(mut clipboard) = arboard::Clipboard::new()
        && clipboard.set_text(text).is_ok()
    {
        success = true;
    }

    // 同时输出 ANSI OSC 52 转义序列，支持远程终端及 SSH 剪贴板透传
    let b64 = base64_encode(text.as_bytes());
    let osc52 = format!("\x1b]52;c;{b64}\x07");
    let mut stdout = std::io::stdout();
    let _ = std::io::Write::write_all(&mut stdout, osc52.as_bytes());
    let _ = std::io::Write::flush(&mut stdout);

    if success {
        Ok(())
    } else {
        Err("Failed to set system clipboard".to_string())
    }
}

/// 从系统剪贴板读取文本
pub fn get_clipboard_text() -> Option<String> {
    if let Ok(mut clipboard) = arboard::Clipboard::new()
        && let Ok(text) = clipboard.get_text()
        && !text.is_empty()
    {
        Some(text)
    } else {
        None
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut res = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        res.push(CHARSET[(b0 >> 2) as usize] as char);
        res.push(CHARSET[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            res.push(CHARSET[(((b1 & 0xf) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            res.push('=');
        }
        if chunk.len() > 2 {
            res.push(CHARSET[(b2 & 0x3f) as usize] as char);
        } else {
            res.push('=');
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SessionStore;

    #[tokio::test]
    async fn test_slash_command_model_execution() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("init_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 无参 /model 打开 Models 模态选择框
        app.set_input("/model");
        app.submit_input().await;
        assert_eq!(app.modal, ActiveModal::Models);
        assert!(app.models_modal.is_some());

        // 2. 带参 /model 切换模型
        app.set_input("/model target_model");
        app.submit_input().await;
        let curr = app
            .active_session
            .session
            .read()
            .await
            .current_model
            .clone();
        assert_eq!(curr, "target_model");
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } => text.contains("Switched model to 'target_model'"),
            _ => false,
        }));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_slash_command_variant_execution() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_app_variant_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("init_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 无参 /variant 打开 Variants 模态框，且支持上下键选择
        app.set_input("/variant");
        app.submit_input().await;
        assert_eq!(app.modal, ActiveModal::Variants);
        assert!(app.variants_modal.is_some());
        let vm = app.variants_modal.as_ref().unwrap();
        assert!(!vm.variants.is_empty());
        assert_eq!(vm.variants[0].id, None); // 首项为 default / none

        // 2. 带参 /variant 切换推理档位
        app.set_input("/variant high");
        app.submit_input().await;
        let curr = app
            .active_session
            .session
            .read()
            .await
            .current_variant
            .clone();
        assert_eq!(curr, Some("high".to_string()));
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } => text.contains("Set reasoning variant to 'high'"),
            _ => false,
        }));

        // 3. /variant none 重置为 None
        app.set_input("/variant none");
        app.submit_input().await;
        let curr_none = app
            .active_session
            .session
            .read()
            .await
            .current_variant
            .clone();
        assert_eq!(curr_none, None);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_slash_command_thinking_execution() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_app_thinking_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("test_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        assert!(!app.thinking_concise);

        // 执行 /thinking 切换为 concise
        app.set_input("/thinking");
        app.submit_input().await;
        assert!(app.thinking_concise);
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } =>
                text.contains("Thinking display mode set to concise"),
            _ => false,
        }));

        // 再次执行 /think 切换回 full
        app.set_input("/think");
        app.submit_input().await;
        assert!(!app.thinking_concise);
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } => text.contains("Thinking display mode set to full"),
            _ => false,
        }));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_app_scroll_control_and_clamping() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_app_scroll_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("test_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 初始状态：无滚动条，处于自动吸底模式
        assert_eq!(app.max_scroll, 0);
        assert!(app.auto_scroll);
        app.scroll_up(5);
        assert!(app.auto_scroll, "When max_scroll is 0, scroll_up should not break auto_scroll");

        // 视口更新：内容超出，总可滚动 50 行
        app.update_viewport(50, 20);
        assert_eq!(app.scroll_offset, 50);
        assert!(app.auto_scroll);

        // 向上滚动 5 行
        app.scroll_up(5);
        assert!(!app.auto_scroll);
        assert_eq!(app.scroll_offset, 45);

        // 向上滚动超过顶部时在 0 处截断
        app.scroll_up(100);
        assert_eq!(app.scroll_offset, 0);

        // 向下滚动 10 行
        app.scroll_down(10);
        assert_eq!(app.scroll_offset, 10);
        assert!(!app.auto_scroll);

        // 向下滚动超过最大可滚动行，恢复吸底状态并精确 clamp 于 max_scroll
        app.scroll_down(100);
        assert_eq!(app.scroll_offset, 50);
        assert!(app.auto_scroll);

        // 一键到顶
        app.scroll_to_top();
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.auto_scroll);

        // 一键到底
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 50);
        assert!(app.auto_scroll);

        // 滚动条比例点击与拖拽测试
        app.scroll_to_ratio(0.0);
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.auto_scroll);

        app.scroll_to_ratio(0.5);
        assert_eq!(app.scroll_offset, 25);
        assert!(!app.auto_scroll);

        app.scroll_to_ratio(1.0);
        assert_eq!(app.scroll_offset, 50);
        assert!(app.auto_scroll);

        // 越界拖拽安全测试
        app.scroll_to_ratio(-0.5);
        assert_eq!(app.scroll_offset, 0);

        app.scroll_to_ratio(2.0);
        assert_eq!(app.scroll_offset, 50);
        assert!(app.auto_scroll);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_session_resume_preserves_web_search_state() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_app_resume_search_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store.clone(), None, temp_dir.clone()));

        // 1. 创建 Session 1 并发送首条消息落盘，初始开启 web_search
        let s1 = session_mgr
            .create_session("model_1".into(), None, Some("Session 1".into()), true)
            .await
            .unwrap();
        session_mgr.submit_prompt(s1.clone(), "init s1").await.unwrap();
        let s1_id = s1.id.clone();

        // 2. 创建 Session 2 并发送首条消息落盘，初始关闭 web_search
        let s2 = session_mgr
            .create_session("model_2".into(), None, Some("Session 2".into()), false)
            .await
            .unwrap();
        session_mgr.submit_prompt(s2.clone(), "init s2").await.unwrap();
        let s2_id = s2.id.clone();

        // 3. 启动 App，以 Session 1 为初始会话
        let mut app = App::new(session_mgr.clone(), s1, false).await;
        // App 继承 Session 1 的 web_search: true
        assert!(app.enable_web_search);

        // 4. 切换到 Session 2
        app.switch_to_session(&s2_id).await.unwrap();
        // App 状态切换为 Session 2 的 web_search: false
        assert!(!app.enable_web_search);

        // 5. 切换回 Session 1
        app.switch_to_session(&s1_id).await.unwrap();
        // 恢复 Session 1 时，状态与上一次保持一致为 true，避免 prompt cache 失效
        assert!(app.enable_web_search);

        // 6. 执行 /web off 显式关闭 Session 1 的网络搜索
        app.set_input("/web off");
        app.submit_input().await;
        assert!(!app.enable_web_search);

        // 校验 Session 1 持久化存储中的 web_search 已同步更新为 false
        let loaded_s1 = store.load_session(&s1_id).await.unwrap().unwrap();
        assert!(!loaded_s1.web_search);

        // 7. 再次执行 /web on 开启 Session 1 的网络搜索
        app.set_input("/web on");
        app.submit_input().await;
        assert!(app.enable_web_search);

        let loaded_s1_again = store.load_session(&s1_id).await.unwrap().unwrap();
        assert!(loaded_s1_again.web_search);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_selection_state_lifecycle() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_sel_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("init_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 初始状态选区为空
        assert!(app.text_selection.is_none());
        assert!(!app.is_selecting_text);

        // 2. 开始拖选
        app.start_text_selection(2, 5);
        assert!(app.is_selecting_text);
        assert_eq!(
            app.text_selection,
            Some(TextSelection::new(
                TextCoord { line: 2, col: 5 },
                TextCoord { line: 2, col: 5 }
            ))
        );

        // 3. 拖拽移动游标
        app.update_text_selection(3, 10);
        assert_eq!(
            app.text_selection,
            Some(TextSelection::new(
                TextCoord { line: 2, col: 5 },
                TextCoord { line: 3, col: 10 }
            ))
        );

        // 4. 释放鼠标完成拖选
        app.finish_text_selection();
        assert!(!app.is_selecting_text);
        assert!(app.text_selection.is_some());

        // 5. 单击（起止点相同）释放时自动清空选区
        app.start_text_selection(4, 2);
        app.finish_text_selection();
        assert!(!app.is_selecting_text);
        assert!(app.text_selection.is_none());

        // 6. 手动清空选区
        app.start_text_selection(1, 0);
        app.update_text_selection(1, 10);
        app.finish_text_selection();
        assert!(app.text_selection.is_some());
        app.clear_text_selection();
        assert!(app.text_selection.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_input_selection_state_and_deletion() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_input_sel_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("init_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 设置输入内容与初始状态
        app.set_input("Hello, 世界!");
        assert!(app.input.selection.is_none());
        assert!(!app.is_selecting_input);

        // 2. 拖拽选区
        app.start_input_selection(2);
        assert!(app.is_selecting_input);
        app.update_input_selection(7);
        app.finish_input_selection();
        assert!(!app.is_selecting_input);
        assert_eq!(app.input.selected_text(), Some("llo, ".to_string()));

        // 3. Backspace 删除选区
        app.backspace();
        assert_eq!(app.input.as_str(), "He世界!");
        assert!(app.input.selection.is_none());

        // 4. 单击（起止点相同）释放时自动清空选区
        app.start_input_selection(2);
        app.finish_input_selection();
        assert!(!app.is_selecting_input);
        assert!(app.input.selection.is_none());

        // 5. 键入字符串替换选区
        app.set_input("foo bar baz");
        app.start_input_selection(4);
        app.update_input_selection(7);
        app.finish_input_selection();
        app.insert_str("qux");
        assert_eq!(app.input.as_str(), "foo qux baz");
        assert!(app.input.selection.is_none());

        // 6. Delete 键删除选区
        app.start_input_selection(4);
        app.update_input_selection(7);
        app.finish_input_selection();
        app.delete();
        assert_eq!(app.input.as_str(), "foo  baz");
        assert!(app.input.selection.is_none());

        // 7. 手动清空输入选区
        app.start_input_selection(0);
        app.update_input_selection(3);
        app.finish_input_selection();
        assert!(app.input.selection.is_some());
        app.clear_input_selection();
        assert!(app.input.selection.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"Hello World"), "SGVsbG8gV29ybGQ=");
    }

    #[tokio::test]
    async fn test_paste_multiline_into_input_buffer_preserves_lines() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_paste_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("test_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        assert!(app.last_key_input_time.is_none());

        // 模拟粘贴一段多行代码
        let multiline_code = "fn main() {\r\n    println!(\"hello\");\r\n}\n";
        app.insert_str(multiline_code);

        // 验证多行完整保留在输入框中，行数正确，末尾规范化为 \n
        assert_eq!(app.input.as_str(), "fn main() {\n    println!(\"hello\");\n}\n");
        assert_eq!(app.input_line_count(), 4);
        assert!(app.last_key_input_time.is_some());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_input_time_updated_on_char_insert() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_time_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("test_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        assert!(app.last_key_input_time.is_none());
        app.insert_char('a');
        let t1 = app.last_key_input_time;
        assert!(t1.is_some());

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        app.insert_char('b');
        let t2 = app.last_key_input_time;
        assert!(t2 > t1);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_prompt_history_rotation_and_draft_restore() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_history_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("test_model".into(), None, None, false)
            .await
            .unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 提交两次 Prompt
        app.set_input("first prompt");
        app.submit_input().await;

        app.set_input("second prompt");
        app.submit_input().await;

        assert_eq!(app.prompt_history.len(), 2);
        assert_eq!(app.prompt_history[0], "first prompt");
        assert_eq!(app.prompt_history[1], "second prompt");

        // 2. 当前输入半句草稿
        app.set_input("my draft");

        // 3. 按 Up：轮换到上一条历史 (second prompt)，且当前草稿保存
        app.history_previous();
        assert_eq!(app.input.as_str(), "second prompt");
        assert_eq!(app.saved_draft, "my draft");

        // 4. 再按 Up：轮换到更早的历史 (first prompt)
        app.history_previous();
        assert_eq!(app.input.as_str(), "first prompt");

        // 5. 按 Down：回到 second prompt
        app.history_next();
        assert_eq!(app.input.as_str(), "second prompt");

        // 6. 再按 Down：超出历史范围，恢复草稿 "my draft"
        app.history_next();
        assert_eq!(app.input.as_str(), "my draft");
        assert!(app.history_index.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
