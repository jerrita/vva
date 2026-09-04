use std::path::Path;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::config::model::ModelCostConfig;
use crate::config::AppConfig;
use crate::harness::AgentEvent;
use crate::model::{Annotation, ContentPart, Session, SessionSummary, Turn};
use crate::ui::command::{match_commands, SlashCommand};
use crate::ui::session_manager::{ManagedSession, SessionManager};

#[derive(Debug, Clone)]
pub enum ChatEntry {
    UserPrompt {
        text: String,
    },
    Thinking {
        content: String,
    },
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        result: String,
        is_error: bool,
    },
    AssistantMessage {
        text: String,
        citations: Vec<(String, String)>, // title, url
    },
    SystemNotice {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectScope {
    CurrentProject,
    AllProjects,
}

#[derive(Debug, Clone)]
pub struct SessionsModalState {
    pub scope: ProjectScope,
    pub all_sessions: Vec<SessionSummary>,
    pub filtered_sessions: Vec<SessionSummary>,
    pub selected_index: usize,
}

impl SessionsModalState {
    pub fn new(all_sessions: Vec<SessionSummary>, current_workspace: &Path) -> Self {
        let mut state = Self {
            scope: ProjectScope::CurrentProject,
            all_sessions,
            filtered_sessions: Vec::new(),
            selected_index: 0,
        };
        state.apply_filter(current_workspace);
        state
    }

    pub fn toggle_scope(&mut self, current_workspace: &Path) {
        self.scope = match self.scope {
            ProjectScope::CurrentProject => ProjectScope::AllProjects,
            ProjectScope::AllProjects => ProjectScope::CurrentProject,
        };
        self.apply_filter(current_workspace);
        self.selected_index = 0;
    }

    pub fn apply_filter(&mut self, current_workspace: &Path) {
        let norm_current = std::fs::canonicalize(current_workspace)
            .unwrap_or_else(|_| current_workspace.to_path_buf());

        self.filtered_sessions = match self.scope {
            ProjectScope::CurrentProject => self
                .all_sessions
                .iter()
                .filter(|s| {
                    let norm_s = std::fs::canonicalize(&s.workspace_root)
                        .unwrap_or_else(|_| s.workspace_root.clone());
                    norm_s == norm_current
                })
                .cloned()
                .collect(),
            ProjectScope::AllProjects => self.all_sessions.clone(),
        };

        if self.selected_index >= self.filtered_sessions.len() && !self.filtered_sessions.is_empty() {
            self.selected_index = self.filtered_sessions.len() - 1;
        }
    }

    pub fn next(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.filtered_sessions.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_sessions.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.filtered_sessions.get(self.selected_index)
    }
}

#[derive(Debug, Clone)]
pub struct ModelItem {
    pub full_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub provider_type: String,
    pub reasoning: bool,
    pub context_limit: Option<usize>,
    pub variants: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct ModelsModalState {
    pub models: Vec<ModelItem>,
    pub selected_index: usize,
}

impl ModelsModalState {
    pub fn new(models: Vec<ModelItem>, current_model: &str) -> Self {
        let selected_index = models
            .iter()
            .position(|m| m.full_id == current_model)
            .unwrap_or(0);

        Self {
            models,
            selected_index,
        }
    }

    pub fn next(&mut self) {
        if !self.models.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.models.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.models.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.models.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    pub fn selected_model(&self) -> Option<&ModelItem> {
        self.models.get(self.selected_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantItem {
    pub id: Option<String>,
    pub display_name: String,
    pub description: String,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct VariantsModalState {
    pub current_model: String,
    pub active_variant: Option<String>,
    pub variants: Vec<VariantItem>,
    pub selected_index: usize,
}

impl VariantsModalState {
    pub fn new(
        current_model: String,
        active_variant: Option<String>,
        variants: Vec<VariantItem>,
    ) -> Self {
        let selected_index = variants
            .iter()
            .position(|v| v.id == active_variant)
            .unwrap_or(0);

        Self {
            current_model,
            active_variant,
            variants,
            selected_index,
        }
    }

    pub fn next(&mut self) {
        if !self.variants.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.variants.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.variants.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.variants.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    pub fn selected_variant(&self) -> Option<&VariantItem> {
        self.variants.get(self.selected_index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigItemKind {
    Model,
    Variant,
    WebSearch,
    ThinkingDisplay,
}

#[derive(Debug, Clone)]
pub struct ConfigSettingItem {
    pub kind: ConfigItemKind,
    pub name: String,
    pub current_value: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ConfigModalState {
    pub items: Vec<ConfigSettingItem>,
    pub selected_index: usize,
}

impl ConfigModalState {
    pub fn new(
        current_model: String,
        current_variant: Option<String>,
        web_search: bool,
        thinking_concise: bool,
    ) -> Self {
        let items = vec![
            ConfigSettingItem {
                kind: ConfigItemKind::Model,
                name: "Active Model".to_string(),
                current_value: if current_model.is_empty() { "default".into() } else { current_model },
                description: "Select target model from configured providers".to_string(),
            },
            ConfigSettingItem {
                kind: ConfigItemKind::Variant,
                name: "Reasoning Effort".to_string(),
                current_value: current_variant.unwrap_or_else(|| "none".into()),
                description: "Select reasoning variant / thinking effort level".to_string(),
            },
            ConfigSettingItem {
                kind: ConfigItemKind::WebSearch,
                name: "Web Search Grounding".to_string(),
                current_value: if web_search { "Enabled".into() } else { "Disabled".into() },
                description: "Toggle native search grounding for models".to_string(),
            },
            ConfigSettingItem {
                kind: ConfigItemKind::ThinkingDisplay,
                name: "Thinking Display".to_string(),
                current_value: if thinking_concise { "Concise (1 line)".into() } else { "Full".into() },
                description: "Toggle thinking view (/thinking): concise vs full".to_string(),
            },
        ];

        Self {
            items,
            selected_index: 0,
        }
    }

    pub fn update_web_search(&mut self, web_search: bool) {
        if let Some(item) = self.items.iter_mut().find(|it| it.kind == ConfigItemKind::WebSearch) {
            item.current_value = if web_search { "Enabled".into() } else { "Disabled".into() };
        }
    }

    pub fn update_thinking_display(&mut self, thinking_concise: bool) {
        if let Some(item) = self.items.iter_mut().find(|it| it.kind == ConfigItemKind::ThinkingDisplay) {
            item.current_value = if thinking_concise { "Concise (1 line)".into() } else { "Full".into() };
        }
    }

    pub fn next(&mut self) {
        if !self.items.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.items.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.items.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.items.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    pub fn selected_item(&self) -> Option<&ConfigSettingItem> {
        self.items.get(self.selected_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveModal {
    None,
    Sessions,
    Models,
    Variants,
    Config,
    Help,
}

/// 聚合统计侧边栏数据
#[derive(Debug, Default, Clone)]
pub struct SidebarStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub context_limit: Option<u64>,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub estimated_cost_usd: f64,
    pub total_turns: usize,
}

pub struct App {
    pub session_mgr: Arc<SessionManager>,
    pub active_session: Arc<ManagedSession>,
    pub event_rx: broadcast::Receiver<AgentEvent>,
    pub chat_entries: Vec<ChatEntry>,
    pub input: String,
    pub cursor_pos: usize,
    pub enable_web_search: bool,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
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
    pub input_scroll_y: std::cell::Cell<usize>,
    pub input_scroll_x: std::cell::Cell<usize>,
}

impl App {
    pub async fn new(
        session_mgr: Arc<SessionManager>,
        initial_session: Arc<ManagedSession>,
        enable_web_search: bool,
    ) -> Self {
        let event_rx = initial_session.event_tx.subscribe();
        let mut app = Self {
            session_mgr,
            active_session: initial_session,
            event_rx,
            chat_entries: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            enable_web_search,
            scroll_offset: 0,
            auto_scroll: true,
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
            input_scroll_y: std::cell::Cell::new(0),
            input_scroll_x: std::cell::Cell::new(0),
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
        self.auto_scroll = true;
    }

    /// 切换当前活跃 Session（单例保持，不干扰原 session 后台工作）
    pub async fn switch_to_session(&mut self, session_id: &str) -> Result<(), String> {
        let target = self.session_mgr.get_or_load(session_id).await?;
        self.active_session = target;
        self.event_rx = self.active_session.event_tx.subscribe();
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
            let _ = self.session_mgr.store().save_header(&s_ref).await;
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
                    ("minimal", "Minimal reasoning effort (fastest response, light thinking)"),
                    ("low", "Low reasoning effort (quick thinking for simpler tasks)"),
                    ("medium", "Medium reasoning effort (balanced reasoning and speed)"),
                    ("high", "High reasoning effort (deep thinking for complex coding tasks)"),
                    ("xhigh", "Extra high reasoning effort (extended exploration)"),
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
                ("minimal", "Minimal reasoning effort (fastest response, light thinking)"),
                ("low", "Low reasoning effort (quick thinking for simpler tasks)"),
                ("medium", "Medium reasoning effort (balanced reasoning and speed)"),
                ("high", "High reasoning effort (deep thinking for complex coding tasks)"),
                ("xhigh", "Extra high reasoning effort (extended exploration)"),
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
            && !items.iter().any(|it| it.id.as_deref() == Some(cur.as_str())) {
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
            && let Err(e) = cfg.resolve_model(&current_model, Some(v)) {
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
            let _ = self.session_mgr.store().save_header(&s_ref).await;
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

    /// 用户键盘输入字符 (支持多字节 Unicode/中文字符)
    pub fn insert_char(&mut self, c: char) {
        let byte_offset = self
            .input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_pos)
            .unwrap_or(self.input.len());
        self.input.insert(byte_offset, c);
        self.cursor_pos += 1;
        self.update_command_matches();
    }

    /// 用户粘贴或批量插入字符串 (支持多行与多字节 Unicode/中文字符，自动归一化换行符)
    pub fn insert_str(&mut self, s: &str) {
        let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
        let byte_offset = self
            .input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_pos)
            .unwrap_or(self.input.len());
        self.input.insert_str(byte_offset, &normalized);
        self.cursor_pos += normalized.chars().count();
        self.update_command_matches();
    }

    /// 删除字符 (Backspace)
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 && !self.input.is_empty() {
            self.cursor_pos -= 1;
            if let Some((byte_offset, _)) = self.input.char_indices().nth(self.cursor_pos) {
                self.input.remove(byte_offset);
            }
            self.update_command_matches();
        }
    }

    /// 删除字符 (Delete)
    pub fn delete(&mut self) {
        let char_count = self.input.chars().count();
        if self.cursor_pos < char_count {
            if let Some((byte_offset, _)) = self.input.char_indices().nth(self.cursor_pos) {
                self.input.remove(byte_offset);
            }
            self.update_command_matches();
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.input.chars().count() {
            self.cursor_pos += 1;
        }
    }

    /// 计算输入框文本行数
    pub fn input_line_count(&self) -> usize {
        if self.input.is_empty() {
            1
        } else {
            self.input.split('\n').count()
        }
    }

    /// 计算光标当前所在的行号 (0-indexed) 以及在该行中的显示列宽度 (0-indexed，支持全角 CJK 字符)
    pub fn cursor_line_and_col(&self) -> (usize, usize) {
        let byte_offset = self
            .input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_pos)
            .unwrap_or(self.input.len());
        let prefix = &self.input[..byte_offset];
        let lines: Vec<&str> = prefix.split('\n').collect();
        let cursor_line = lines.len().saturating_sub(1);
        let current_line_prefix = lines.last().copied().unwrap_or("");
        let cursor_col = ratatui::text::Span::raw(current_line_prefix).width();
        (cursor_line, cursor_col)
    }

    /// 光标向上移动一行，在上一行中尽量对齐当前列的视觉显示宽度
    pub fn move_cursor_up(&mut self) {
        if self.input.is_empty() {
            return;
        }
        let all_lines: Vec<&str> = self.input.split('\n').collect();
        if all_lines.len() <= 1 {
            return;
        }

        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1; // +1 对应 '\n'
        }

        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };

        if cursor_row > 0 {
            let target_row = cursor_row - 1;
            let target_line = all_lines[target_row];
            let (_, current_visual_col) = self.cursor_line_and_col();
            let target_char_col = char_idx_for_visual_width(target_line, current_visual_col);
            self.cursor_pos = line_starts[target_row] + target_char_col;
        }
    }

    /// 光标向下移动一行，在下一行中尽量对齐当前列的视觉显示宽度
    pub fn move_cursor_down(&mut self) {
        if self.input.is_empty() {
            return;
        }
        let all_lines: Vec<&str> = self.input.split('\n').collect();
        if all_lines.len() <= 1 {
            return;
        }

        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1; // +1 对应 '\n'
        }

        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };

        if cursor_row + 1 < all_lines.len() {
            let target_row = cursor_row + 1;
            let target_line = all_lines[target_row];
            let (_, current_visual_col) = self.cursor_line_and_col();
            let target_char_col = char_idx_for_visual_width(target_line, current_visual_col);
            self.cursor_pos = line_starts[target_row] + target_char_col;
        }
    }

    /// 光标移动到当前行行首；若已在当前行行首，则移动到整个文本最开头
    pub fn move_cursor_home(&mut self) {
        if self.input.is_empty() || self.cursor_pos == 0 {
            self.cursor_pos = 0;
            return;
        }
        let all_lines: Vec<&str> = self.input.split('\n').collect();
        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1;
        }
        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        let line_start = line_starts[cursor_row];
        if self.cursor_pos == line_start {
            self.cursor_pos = 0;
        } else {
            self.cursor_pos = line_start;
        }
    }

    /// 光标移动到当前行行尾；若已在当前行行尾，则移动到整个文本最末尾
    pub fn move_cursor_end(&mut self) {
        let total_chars = self.input.chars().count();
        if self.input.is_empty() || self.cursor_pos >= total_chars {
            self.cursor_pos = total_chars;
            return;
        }
        let all_lines: Vec<&str> = self.input.split('\n').collect();
        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1;
        }
        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        let line_end = line_starts[cursor_row] + all_lines[cursor_row].chars().count();
        if self.cursor_pos == line_end {
            self.cursor_pos = total_chars;
        } else {
            self.cursor_pos = line_end;
        }
    }

    /// 检查并更新输入框对应的斜杠命令匹配
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

    /// 命令浮层向上高亮切换
    pub fn command_select_previous(&mut self) {
        if !self.matched_commands.is_empty() {
            self.selected_command_index = if self.selected_command_index == 0 {
                self.matched_commands.len() - 1
            } else {
                self.selected_command_index - 1
            };
        }
    }

    /// 命令浮层向下高亮切换
    pub fn command_select_next(&mut self) {
        if !self.matched_commands.is_empty() {
            self.selected_command_index =
                (self.selected_command_index + 1) % self.matched_commands.len();
        }
    }

    /// 使用当前选中的命令自动补全输入框
    pub fn complete_selected_command(&mut self) {
        if let Some(cmd) = self.matched_commands.get(self.selected_command_index) {
            self.input = format!("{} ", cmd.name);
            self.cursor_pos = self.input.chars().count();
            self.input_scroll_y.set(0);
            self.input_scroll_x.set(0);
            self.update_command_matches();
        }
    }

    /// 执行用户当前输入框指令（普通提问或执行斜杠命令）
    pub async fn submit_input(&mut self) {
        let input_text = self.input.trim().to_string();
        if input_text.is_empty() {
            return;
        }

        self.input.clear();
        self.cursor_pos = 0;
        self.input_scroll_y.set(0);
        self.input_scroll_x.set(0);
        self.matched_commands.clear();
        self.selected_command_index = 0;

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
            .submit_prompt(self.active_session.clone(), &input_text, self.enable_web_search)
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
                let mode_str = if self.thinking_concise { "concise (1 line)" } else { "full" };
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
                    self.chat_entries.push(ChatEntry::Thinking {
                        content: thought,
                    });
                }
            }
            AgentEvent::ToolExecuting { name, args } => {
                self.chat_entries.push(ChatEntry::ToolCall { name, args });
            }
            AgentEvent::ToolFinished { name, result, is_error } => {
                self.chat_entries.push(ChatEntry::ToolResult {
                    name,
                    result,
                    is_error,
                });
            }
            AgentEvent::Text(text) => {
                if let Some(ChatEntry::AssistantMessage { text: existing, .. }) = self.chat_entries.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.chat_entries.push(ChatEntry::AssistantMessage {
                        text,
                        citations: Vec::new(),
                    });
                }
            }
            AgentEvent::Citation { title, url } => {
                if let Some(ChatEntry::AssistantMessage { citations, .. }) = self.chat_entries.last_mut()
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
                self.chat_entries.push(ChatEntry::SystemNotice {
                    text: err_msg,
                });
            }
        }
        if self.auto_scroll {
            self.scroll_offset = 0;
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

/// 计算侧边栏统计信息
pub fn compute_stats(session: &Session, app_config: Option<&AppConfig>) -> SidebarStats {
    let mut prompt_tok = 0u64;
    let mut comp_tok = 0u64;
    let mut tot_tok = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;

    for turn in &session.turns {
        if let Some(usage) = &turn.usage {
            prompt_tok += usage.prompt_tokens as u64;
            comp_tok += usage.completion_tokens as u64;
            tot_tok += usage.total_tokens as u64;
        }
        if let Some(anthropic) = &turn.meta.anthropic {
            if let Some(r) = anthropic.cache_read_input_tokens {
                cache_read += r as u64;
            }
            if let Some(w) = anthropic.cache_creation_input_tokens {
                cache_write += w as u64;
            }
        }
    }

    let mut context_limit = None;
    let mut cost_cfg: Option<ModelCostConfig> = None;

    if let Some(cfg) = app_config
        && let Ok(resolved) = cfg.resolve_model(&session.current_model, session.current_variant.as_deref()) {
            context_limit = resolved.model_config.limit.as_ref().map(|l| l.context as u64);
            cost_cfg = resolved.model_config.cost.clone();
        }

    let mut estimated_cost = 0.0;
    if let Some(cost) = cost_cfg {
        let in_cost = (prompt_tok as f64) * (cost.input / 1_000_000.0);
        let out_cost = (comp_tok as f64) * (cost.output / 1_000_000.0);
        let read_cost = (cache_read as f64) * (cost.cache_read.unwrap_or(0.0) / 1_000_000.0);
        let write_cost = (cache_write as f64) * (cost.cache_write.unwrap_or(0.0) / 1_000_000.0);
        estimated_cost = in_cost + out_cost + read_cost + write_cost;
    }

    SidebarStats {
        prompt_tokens: prompt_tok,
        completion_tokens: comp_tok,
        total_tokens: tot_tok,
        context_limit,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        estimated_cost_usd: estimated_cost,
        total_turns: session.turns.len(),
    }
}

/// 将历史 Turn 数据解析为 UI 展现列表
pub fn entries_from_turns(turns: &[Turn]) -> Vec<ChatEntry> {
    let mut entries = Vec::new();

    for turn in turns {
        match turn.role {
            crate::model::Role::User => {
                for msg in &turn.messages {
                    let mut text_acc = String::new();
                    for part in &msg.parts {
                        if let ContentPart::Text { text, .. } = part {
                            if !text_acc.is_empty() {
                                text_acc.push('\n');
                            }
                            text_acc.push_str(text);
                        }
                    }
                    if !text_acc.is_empty() {
                        entries.push(ChatEntry::UserPrompt { text: text_acc });
                    }
                }
            }
            crate::model::Role::Assistant => {
                for msg in &turn.messages {
                    for part in &msg.parts {
                        match part {
                            ContentPart::Reasoning { content, .. } => {
                                entries.push(ChatEntry::Thinking {
                                    content: content.clone(),
                                });
                            }
                            ContentPart::Text { text, annotations } => {
                                let mut citations = Vec::new();
                                for ann in annotations {
                                    if let Annotation::UrlCitation { title, url, .. } = ann {
                                        citations.push((title.clone(), url.clone()));
                                    }
                                }
                                entries.push(ChatEntry::AssistantMessage {
                                    text: text.clone(),
                                    citations,
                                });
                            }
                            ContentPart::ToolCall(call) => {
                                entries.push(ChatEntry::ToolCall {
                                    name: call.name.clone(),
                                    args: call.arguments.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            crate::model::Role::Tool => {
                for msg in &turn.messages {
                    for part in &msg.parts {
                        if let ContentPart::ToolResult(res) = part {
                            entries.push(ChatEntry::ToolResult {
                                name: res.tool_call_id.clone(),
                                result: res.content.clone(),
                                is_error: res.is_error,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use chrono::Utc;

    #[test]
    fn test_sessions_modal_filter_and_toggle() {
        let ws_a = PathBuf::from("/home/user/project_a");
        let ws_b = PathBuf::from("/home/user/project_b");

        let s1 = SessionSummary {
            id: "sess_1".into(),
            title: Some("Task A1".into()),
            workspace_root: ws_a.clone(),
            created_at: Utc::now(),
            turn_count: 2,
            current_model: "model_a".into(),
            current_variant: None,
        };

        let s2 = SessionSummary {
            id: "sess_2".into(),
            title: Some("Task A2".into()),
            workspace_root: ws_a.clone(),
            created_at: Utc::now(),
            turn_count: 5,
            current_model: "model_a".into(),
            current_variant: None,
        };

        let s3 = SessionSummary {
            id: "sess_3".into(),
            title: Some("Task B1".into()),
            workspace_root: ws_b.clone(),
            created_at: Utc::now(),
            turn_count: 1,
            current_model: "model_b".into(),
            current_variant: None,
        };

        let mut modal = SessionsModalState::new(vec![s1, s2, s3], &ws_a);

        // 默认按当前工程过滤
        assert_eq!(modal.scope, ProjectScope::CurrentProject);
        assert_eq!(modal.filtered_sessions.len(), 2);
        assert_eq!(modal.selected_session().unwrap().id, "sess_1");

        // 导航测试
        modal.next();
        assert_eq!(modal.selected_session().unwrap().id, "sess_2");
        modal.next();
        assert_eq!(modal.selected_session().unwrap().id, "sess_1"); // wrap around
        modal.previous();
        assert_eq!(modal.selected_session().unwrap().id, "sess_2");

        // 切换到 All Projects
        modal.toggle_scope(&ws_a);
        assert_eq!(modal.scope, ProjectScope::AllProjects);
        assert_eq!(modal.filtered_sessions.len(), 3);

        // 再次切换回 Current Project
        modal.toggle_scope(&ws_a);
        assert_eq!(modal.scope, ProjectScope::CurrentProject);
        assert_eq!(modal.filtered_sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_slash_command_model_execution() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr.create_session("init_model".into(), None, None).await.unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 无参 /model 打开 Models 模态选择框
        app.input = "/model".into();
        app.submit_input().await;
        assert_eq!(app.modal, ActiveModal::Models);
        assert!(app.models_modal.is_some());

        // 2. 带参 /model 切换模型
        app.input = "/model target_model".into();
        app.submit_input().await;
        let curr = app.active_session.session.read().await.current_model.clone();
        assert_eq!(curr, "target_model");
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } => text.contains("Switched model to 'target_model'"),
            _ => false,
        }));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_models_modal_navigation() {
        let m1 = ModelItem {
            full_id: "prov/model-1".into(),
            provider_id: "prov".into(),
            model_id: "model-1".into(),
            display_name: "Model One".into(),
            provider_type: "response".into(),
            reasoning: true,
            context_limit: Some(128000),
            variants: Some(vec!["low".into(), "high".into()]),
        };
        let m2 = ModelItem {
            full_id: "prov/model-2".into(),
            provider_id: "prov".into(),
            model_id: "model-2".into(),
            display_name: "Model Two".into(),
            provider_type: "anthropic".into(),
            reasoning: false,
            context_limit: Some(200000),
            variants: None,
        };

        let mut modal = ModelsModalState::new(vec![m1, m2], "prov/model-1");
        assert_eq!(modal.selected_index, 0);
        assert_eq!(modal.selected_model().unwrap().full_id, "prov/model-1");

        modal.next();
        assert_eq!(modal.selected_index, 1);
        assert_eq!(modal.selected_model().unwrap().full_id, "prov/model-2");

        modal.next();
        assert_eq!(modal.selected_index, 0); // wrap around

        modal.previous();
        assert_eq!(modal.selected_index, 1);
    }

    #[test]
    fn test_variants_modal_navigation() {
        let v1 = VariantItem {
            id: None,
            display_name: "none (default)".into(),
            description: "Default reasoning".into(),
            is_default: true,
        };
        let v2 = VariantItem {
            id: Some("low".into()),
            display_name: "low".into(),
            description: "Low effort".into(),
            is_default: false,
        };
        let v3 = VariantItem {
            id: Some("high".into()),
            display_name: "high".into(),
            description: "High effort".into(),
            is_default: false,
        };

        // 默认定位到 active_variant (high)
        let mut modal = VariantsModalState::new(
            "prov/model-1".into(),
            Some("high".into()),
            vec![v1, v2, v3],
        );
        assert_eq!(modal.selected_index, 2);
        assert_eq!(modal.selected_variant().unwrap().id.as_deref(), Some("high"));

        modal.next();
        assert_eq!(modal.selected_index, 0); // wrap around to none
        assert_eq!(modal.selected_variant().unwrap().id, None);

        modal.previous();
        assert_eq!(modal.selected_index, 2); // wrap around back to high

        modal.previous();
        assert_eq!(modal.selected_index, 1); // low
        assert_eq!(modal.selected_variant().unwrap().id.as_deref(), Some("low"));
    }

    #[tokio::test]
    async fn test_slash_command_variant_execution() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_variant_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr.create_session("init_model".into(), None, None).await.unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 无参 /variant 打开 Variants 模态框，且支持上下键选择
        app.input = "/variant".into();
        app.submit_input().await;
        assert_eq!(app.modal, ActiveModal::Variants);
        assert!(app.variants_modal.is_some());
        let vm = app.variants_modal.as_ref().unwrap();
        assert!(!vm.variants.is_empty());
        assert_eq!(vm.variants[0].id, None); // 首项为 default / none

        // 2. 带参 /variant 切换推理档位
        app.input = "/variant high".into();
        app.submit_input().await;
        let curr = app.active_session.session.read().await.current_variant.clone();
        assert_eq!(curr, Some("high".to_string()));
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } => text.contains("Set reasoning variant to 'high'"),
            _ => false,
        }));

        // 3. /variant none 重置为 None
        app.input = "/variant none".into();
        app.submit_input().await;
        let curr_none = app.active_session.session.read().await.current_variant.clone();
        assert_eq!(curr_none, None);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_config_modal_navigation_and_toggle() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_config_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr.create_session("test_model".into(), Some("medium".into()), None).await.unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 打开 /config 模态框
        app.input = "/config".into();
        app.submit_input().await;
        assert_eq!(app.modal, ActiveModal::Config);
        assert!(app.config_modal.is_some());

        let cm = app.config_modal.as_mut().unwrap();
        assert_eq!(cm.items.len(), 4);
        assert_eq!(cm.items[0].kind, ConfigItemKind::Model);
        assert_eq!(cm.items[1].kind, ConfigItemKind::Variant);
        assert_eq!(cm.items[2].kind, ConfigItemKind::WebSearch);
        assert_eq!(cm.items[2].current_value, "Disabled");
        assert_eq!(cm.items[3].kind, ConfigItemKind::ThinkingDisplay);
        assert_eq!(cm.items[3].current_value, "Full");

        // 导航
        cm.next();
        assert_eq!(cm.selected_index, 1);
        cm.next();
        assert_eq!(cm.selected_index, 2);
        cm.update_web_search(true);
        assert_eq!(cm.items[2].current_value, "Enabled");
        cm.next();
        assert_eq!(cm.selected_index, 3);
        cm.update_thinking_display(true);
        assert_eq!(cm.items[3].current_value, "Concise (1 line)");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_slash_command_thinking_execution() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_thinking_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr.create_session("test_model".into(), None, None).await.unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        assert!(!app.thinking_concise);

        // 执行 /thinking 切换为 concise
        app.input = "/thinking".into();
        app.submit_input().await;
        assert!(app.thinking_concise);
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } => text.contains("Thinking display mode set to concise"),
            _ => false,
        }));

        // 再次执行 /think 切换回 full
        app.input = "/think".into();
        app.submit_input().await;
        assert!(!app.thinking_concise);
        assert!(app.chat_entries.iter().any(|e| match e {
            ChatEntry::SystemNotice { text } => text.contains("Thinking display mode set to full"),
            _ => false,
        }));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_unicode_chinese_input_and_editing() {
        let temp_dir = std::env::temp_dir().join(format!("vva_app_chinese_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr.create_session("test_model".into(), None, None).await.unwrap();
        let mut app = App::new(session_mgr, session, false).await;

        // 1. 连续输入中文字符，严禁触发 byte index not a char boundary panic
        app.insert_char('你');
        app.insert_char('好');
        app.insert_char('世');
        app.insert_char('界');
        assert_eq!(app.input, "你好世界");
        assert_eq!(app.cursor_pos, 4);

        // 2. 光标向左移动到 "你好" 与 "世界" 之间
        app.move_cursor_left();
        app.move_cursor_left();
        assert_eq!(app.cursor_pos, 2);

        // 3. 在中间插入中文字符
        app.insert_char('，');
        assert_eq!(app.input, "你好，世界");
        assert_eq!(app.cursor_pos, 3);

        // 4. Backspace 删除逗号
        app.backspace();
        assert_eq!(app.input, "你好世界");
        assert_eq!(app.cursor_pos, 2);

        // 5. Delete 删除后面的 "世"
        app.delete();
        assert_eq!(app.input, "你好界");
        assert_eq!(app.cursor_pos, 2);

        // 6. 光标 Home 与 End
        app.move_cursor_home();
        assert_eq!(app.cursor_pos, 0);
        app.move_cursor_end();
        assert_eq!(app.cursor_pos, 3);

        // 7. 批量粘贴中文字符串
        app.insert_str("！欢迎使用");
        assert_eq!(app.input, "你好界！欢迎使用");
        assert_eq!(app.cursor_pos, 8);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_multiline_paste_and_line_counting() {
        let temp_dir = std::env::temp_dir().join(format!("vva_ml_test_{}", uuid::Uuid::new_v4()));
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("dummy/model".into(), None, None)
            .await
            .unwrap();

        let mut app = App::new(session_mgr, session, false).await;

        // 1. 初始单行
        assert_eq!(app.input_line_count(), 1);
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 0);
        assert_eq!(col, 0);

        // 2. 粘贴包含 CRLF 与 CR 的多行内容，验证统一归一化为 \n
        app.insert_str("def foo():\r\n    msg = '你好'\r    return msg\n");
        assert_eq!(app.input, "def foo():\n    msg = '你好'\n    return msg\n");
        assert_eq!(app.input_line_count(), 4);

        // 3. 验证光标在第 3 行开头 (第四行，空行)
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 3);
        assert_eq!(col, 0);

        // 4. 测试 Up 向上移动
        app.move_cursor_up();
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 2);
        assert_eq!(col, 0);

        // 5. 移到行尾并测试 visual width 对齐
        app.move_cursor_end();
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 2);
        assert_eq!(col, 14); // "    return msg" 长度 14

        // 向上移动到第 1 行："    msg = '你好'" (视觉宽度 4 + 7 + 4 = 15)
        app.move_cursor_up();
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 1);
        assert!(col >= 13);

        // 向上移动到第 0 行："def foo():" (视觉宽度 10)
        app.move_cursor_up();
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 0);
        assert_eq!(col, 10); // 对齐至行尾

        // 测试 Home 行首与文档首
        app.move_cursor_home();
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 0);
        assert_eq!(col, 0);

        // 向下移动
        app.move_cursor_down();
        let (row, col) = app.cursor_line_and_col();
        assert_eq!(row, 1);
        assert_eq!(col, 0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

/// 根据目标视觉宽度在给定单行文本中寻找最匹配的字符索引
fn char_idx_for_visual_width(line: &str, target_width: usize) -> usize {
    let mut current_width: usize = 0;
    let mut best_idx = 0;
    let mut best_diff = target_width;

    for (char_idx, c) in line.chars().enumerate() {
        let diff = current_width.abs_diff(target_width);
        if diff < best_diff {
            best_diff = diff;
            best_idx = char_idx;
        }
        let w = ratatui::text::Span::raw(c.to_string()).width();
        current_width += w;
        let diff_after = current_width.abs_diff(target_width);
        if diff_after < best_diff {
            best_diff = diff_after;
            best_idx = char_idx + 1;
        }
        if current_width >= target_width {
            break;
        }
    }
    best_idx
}

