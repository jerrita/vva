use super::App;
use crate::ui::history::{ChatEntry, compute_stats, entries_from_turns};
use crate::ui::modals::*;

impl App {
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
        self.clear_sidebar_selection();

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
        if let Some(cfg) = &model_cfg
            && let Some(valid_vars) = &cfg.variants
            && !valid_vars.is_empty()
        {
            items.push(VariantItem {
                id: None,
                display_name: "none (default)".to_string(),
                is_default: true,
            });
            for v in valid_vars {
                items.push(VariantItem {
                    id: Some(v.clone()),
                    display_name: v.clone(),
                    is_default: false,
                });
            }
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

    /// 打开 /config 设置弹窗
    pub async fn open_config_modal(&mut self) {
        let (current_model, current_variant, web_search) = {
            let s = self.active_session.session.read().await;
            (
                s.current_model.clone(),
                s.current_variant.clone(),
                s.web_search,
            )
        };

        self.config_modal = Some(ConfigModalState::new(
            current_model,
            current_variant,
            web_search,
            self.thinking_concise,
        ));
        self.modal = ActiveModal::Config;
    }
}
