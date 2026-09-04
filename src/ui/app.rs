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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveModal {
    None,
    Sessions,
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
    pub matched_commands: Vec<&'static SlashCommand>,
    pub selected_command_index: usize,
    pub stats: SidebarStats,
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
            matched_commands: Vec::new(),
            selected_command_index: 0,
            stats: SidebarStats::default(),
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

    /// 用户键盘输入字符
    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += 1;
        self.update_command_matches();
    }

    /// 删除字符 (Backspace)
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 && !self.input.is_empty() {
            self.cursor_pos -= 1;
            self.input.remove(self.cursor_pos);
            self.update_command_matches();
        }
    }

    /// 删除字符 (Delete)
    pub fn delete(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.input.remove(self.cursor_pos);
            self.update_command_matches();
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos += 1;
        }
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
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
            self.cursor_pos = self.input.len();
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
                    let curr = {
                        let s = self.active_session.session.read().await;
                        s.current_model.clone()
                    };
                    self.chat_entries.push(ChatEntry::SystemNotice {
                        text: format!("Current model: {curr}. Usage: /model <model_id>"),
                    });
                } else {
                    let (sid, current_var) = {
                        let s = self.active_session.session.read().await;
                        (s.id.clone(), s.current_variant.clone())
                    };
                    {
                        let mut s = self.active_session.session.write().await;
                        s.set_model(arg.clone(), current_var);
                    }
                    let s_clone = {
                        let s_ref = self.active_session.session.read().await;
                        let _ = self.session_mgr.store().save_header(&s_ref).await;
                        s_ref.clone()
                    };
                    self.chat_entries.push(ChatEntry::SystemNotice {
                        text: format!("Switched model to '{arg}' for session {sid}"),
                    });
                    self.stats = compute_stats(&s_clone, self.session_mgr.app_config());
                }
            }
            "/variant" | "/v" | "/effort" => {
                if arg.is_empty() {
                    let curr = {
                        let s = self.active_session.session.read().await;
                        s.current_variant.clone().unwrap_or_else(|| "none".into())
                    };
                    self.chat_entries.push(ChatEntry::SystemNotice {
                        text: format!("Current variant: {curr}. Usage: /variant <effort>"),
                    });
                } else {
                    let var = if arg == "none" || arg == "default" {
                        None
                    } else {
                        Some(arg.clone())
                    };
                    let (sid, current_model) = {
                        let s = self.active_session.session.read().await;
                        (s.id.clone(), s.current_model.clone())
                    };
                    {
                        let mut s = self.active_session.session.write().await;
                        s.set_model(current_model, var.clone());
                    }
                    let s_clone = {
                        let s_ref = self.active_session.session.read().await;
                        let _ = self.session_mgr.store().save_header(&s_ref).await;
                        s_ref.clone()
                    };
                    self.chat_entries.push(ChatEntry::SystemNotice {
                        text: format!("Set reasoning variant to '{arg}' for session {sid}"),
                    });
                    self.stats = compute_stats(&s_clone, self.session_mgr.app_config());
                }
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
                self.chat_entries.push(ChatEntry::SystemNotice {
                    text: format!("Native web search is now {status}"),
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
                    content.push('\n');
                    content.push_str(thought.trim());
                } else {
                    self.chat_entries.push(ChatEntry::Thinking {
                        content: thought.trim().to_string(),
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
                    existing.push('\n');
                    existing.push_str(&text);
                } else {
                    self.chat_entries.push(ChatEntry::AssistantMessage {
                        text,
                        citations: Vec::new(),
                    });
                }
            }
            AgentEvent::Finished => {
                let s_clone = {
                    let s = self.active_session.session.read().await;
                    s.clone()
                };
                self.stats = compute_stats(&s_clone, self.session_mgr.app_config());
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

    if let Some(cfg) = app_config {
        if let Ok(resolved) = cfg.resolve_model(&session.current_model, session.current_variant.as_deref()) {
            context_limit = resolved.model_config.limit.as_ref().map(|l| l.context as u64);
            cost_cfg = resolved.model_config.cost.clone();
        }
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
}
