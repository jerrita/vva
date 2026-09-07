use super::App;
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
        self.clear_selections();

        // 处理斜杠命令
        if input_text.starts_with('/') {
            self.handle_slash_command(&input_text).await;
            return;
        }

        // 普通用户 Prompt
        self.auto_scroll = true;

        if let Err(e) = self
            .session_mgr
            .submit_prompt(self.active_session.clone(), &input_text)
            .await
        {
            self.notices.push(format!("Error: {e}"));
        }
        self.refresh_view();
    }

    /// 设置或切换原生网络搜索（/search 命令与 Config 弹窗共用）：
    /// 更新会话字段、持久化 Header、同步弹窗显示并输出系统提示
    pub async fn set_web_search(&mut self, target: Option<bool>) {
        let enabled = target.unwrap_or(!self.view.session.web_search);
        if let Err(error) = self
            .active_session
            .configure(None, None, Some(enabled))
            .await
        {
            self.notices.push(error);
            return;
        }
        self.refresh_view();

        if let ActiveModal::Config(cm) = &mut self.modal {
            cm.update_item_value(
                ConfigItemKind::WebSearch,
                if self.view.session.web_search {
                    "Enabled"
                } else {
                    "Disabled"
                }
                .to_string(),
            );
        }
    }

    /// 切换思考过程展示模式（/thinking 命令与 Config 弹窗共用）
    pub fn toggle_thinking_display(&mut self) {
        self.thinking_concise = !self.thinking_concise;
        if let ActiveModal::Config(cm) = &mut self.modal {
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
            "/new" => {
                if let Err(error) = self.new_session().await {
                    self.notices.push(error);
                }
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
                if let Err(error) = self.session_mgr.delete_session(&self.active_session).await {
                    self.notices.push(error);
                    return;
                }
                if let Err(error) = self.new_session().await {
                    self.notices.push(error);
                }
            }
            "/help" | "/h" | "/?" => {
                self.modal = ActiveModal::Help;
            }
            "/quit" | "/exit" | "/q" => {
                self.should_quit = true;
            }
            _ => {
                self.notices.push(format!(
                    "Unknown command '{cmd}'. Type /help for available commands."
                ));
            }
        }
    }
}
