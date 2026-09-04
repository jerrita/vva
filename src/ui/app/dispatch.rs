use super::App;
use crate::harness::agent::AgentEvent;
use crate::ui::history::{ChatEntry, compute_stats};
use crate::ui::modals::{ActiveModal, ConfigItemKind};

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

    /// 设置或切换原生网络搜索（/search 命令与 Config 弹窗共用）：
    /// 更新会话字段、持久化 Header、同步弹窗显示并输出系统提示
    pub async fn set_web_search(&mut self, target: Option<bool>) {
        self.enable_web_search = target.unwrap_or(!self.enable_web_search);
        {
            let mut s = self.active_session.session.write().await;
            s.set_web_search(self.enable_web_search);
        }
        let _ = self.session_mgr.persist_header(&self.active_session).await;

        let status = if self.enable_web_search { "ON" } else { "OFF" };
        if let Some(ref mut cm) = self.config_modal {
            cm.update_item_value(
                ConfigItemKind::WebSearch,
                if self.enable_web_search {
                    "Enabled"
                } else {
                    "Disabled"
                }
                .to_string(),
            );
        }
        self.chat_entries.push(ChatEntry::SystemNotice {
            text: format!("Native web search is now {status}"),
        });
    }

    /// 切换思考过程展示模式（/thinking 命令与 Config 弹窗共用）
    pub fn toggle_thinking_display(&mut self) {
        self.thinking_concise = !self.thinking_concise;
        let mode_str = if self.thinking_concise {
            "concise (1 line)"
        } else {
            "full"
        };
        if let Some(ref mut cm) = self.config_modal {
            cm.update_item_value(
                ConfigItemKind::ThinkingDisplay,
                if self.thinking_concise {
                    "Concise (1 line)"
                } else {
                    "Full"
                }
                .to_string(),
            );
        }
        self.chat_entries.push(ChatEntry::SystemNotice {
            text: format!("Thinking display mode set to {mode_str}."),
        });
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
                let target = if arg.eq_ignore_ascii_case("on") {
                    Some(true)
                } else if arg.eq_ignore_ascii_case("off") {
                    Some(false)
                } else {
                    None
                };
                self.set_web_search(target).await;
            }
            "/thinking" | "/think" | "/th" => {
                self.toggle_thinking_display();
            }
            "/clear" | "/c" => {
                let (model, variant, web_search) = {
                    let session = self.active_session.session.read().await;
                    (
                        session.current_model.clone(),
                        session.current_variant.clone(),
                        session.web_search,
                    )
                };

                if let Err(error) = self.session_mgr.delete_session(&self.active_session).await {
                    self.chat_entries.push(ChatEntry::SystemNotice {
                        text: format!("Failed to clear session: {error}"),
                    });
                    return;
                }

                match self
                    .session_mgr
                    .create_session(model, variant, None, web_search)
                    .await
                {
                    Ok(session) => {
                        self.active_session = session;
                        self.event_rx = self.active_session.event_tx.subscribe();
                        self.enable_web_search = web_search;
                    }
                    Err(error) => {
                        self.chat_entries.push(ChatEntry::SystemNotice {
                            text: format!("Failed to create new session: {error}"),
                        });
                        return;
                    }
                }
                self.chat_entries.clear();
                self.scroll_offset = 0;
                self.max_scroll = 0;
                self.auto_scroll = true;
                let session = self.active_session.session.read().await.clone();
                self.stats = compute_stats(&session, self.session_mgr.app_config());
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
            AgentEvent::Notice(text) => {
                self.chat_entries.push(ChatEntry::SystemNotice { text });
            }
            AgentEvent::Finished => {
                let s_clone = {
                    let s = self.active_session.session.read().await;
                    s.clone()
                };
                self.stats = compute_stats(&s_clone, self.session_mgr.app_config());
            }
            AgentEvent::Aborted => {
                self.chat_entries.push(ChatEntry::SystemNotice {
                    text: "Turn aborted.".to_string(),
                });
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
