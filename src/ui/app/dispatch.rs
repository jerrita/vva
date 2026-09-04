use super::App;
use crate::harness::agent::AgentEvent;
use crate::ui::history::{ChatEntry, compute_stats};
use crate::ui::modals::ActiveModal;

impl App {
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
        self.clear_sidebar_selection();

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
            .submit_prompt(self.active_session.clone(), &input_text)
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
}
