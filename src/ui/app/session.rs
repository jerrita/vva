use super::App;
use crate::ui::history::compute_stats;
use crate::ui::modals::*;

impl App {
    pub fn refresh_view(&mut self) {
        let view = self.active_session.snapshot();
        if !std::sync::Arc::ptr_eq(&view.session, &self.view.session) {
            self.stats = compute_stats(&view.session, self.session_mgr.app_config());
        }
        // 会话任务的新通知按到达顺序汇入界面通知流
        if view.notices.len() > self.consumed_notices {
            self.notices
                .extend_from_slice(&view.notices[self.consumed_notices..]);
            self.consumed_notices = view.notices.len();
        }
        self.view = view;
    }

    /// 接管一个会话句柄：重置界面局部状态并订阅其视图（切换、新建共用）
    pub fn attach_session(&mut self, session: std::sync::Arc<crate::harness::ManagedSession>) {
        self.active_session = session;
        self.view_rx = self.active_session.subscribe();
        self.clear_selections();
        self.chat.lines.clear();
        self.notices.clear();
        self.consumed_notices = 0;
        self.scroll_offset = 0;
        self.max_scroll = 0;
        self.auto_scroll = true;
        self.refresh_view();
    }

    pub async fn switch_to_session(&mut self, session_id: &str) -> Result<(), String> {
        let session = self.session_mgr.get_or_load(session_id).await?;
        self.attach_session(session);
        Ok(())
    }

    /// 用当前会话的配置创建并接管一个空会话。
    pub async fn new_session(&mut self) -> Result<(), String> {
        let session = self.view.session.clone();
        let handle = self
            .session_mgr
            .create_session(
                session.current_model.clone(),
                session.current_variant.clone(),
                None,
                session.web_search,
            )
            .await?;
        self.attach_session(handle);
        Ok(())
    }

    pub async fn open_sessions_modal(&mut self) {
        match self.session_mgr.store().list_sessions().await {
            Ok(sessions) => {
                self.modal = ActiveModal::Sessions(SessionsModalState::new(
                    sessions,
                    self.session_mgr.workspace(),
                ));
            }
            Err(error) => self.notices.push(error.to_string()),
        }
    }

    pub fn current_session_model(&self) -> String {
        self.view.session.current_model.clone()
    }

    pub async fn open_models_modal(&mut self) {
        let config = self.session_mgr.app_config();
        let mut models = Vec::new();
        for (provider_id, provider) in &config.providers {
            for (model_id, model) in &provider.models {
                models.push(ModelItem {
                    full_id: format!("{provider_id}/{model_id}"),
                    provider_id: provider_id.clone(),
                    model_id: model_id.clone(),
                    display_name: model.name.clone(),
                    provider_type: model
                        .effective_type(provider.provider_type.unwrap_or_default())
                        .as_str()
                        .into(),
                    reasoning: model.reasoning,
                    context_limit: model.limit.as_ref().map(|l| l.context),
                    variants: model.variants.clone(),
                });
            }
        }
        models.sort_by(|a, b| a.full_id.cmp(&b.full_id));
        self.modal = ActiveModal::Models(ModelsModalState::new(
            models,
            &self.view.session.current_model,
        ));
    }

    pub async fn switch_model(&mut self, model: &str) {
        let config = self.session_mgr.app_config();
        let variant = self.view.session.current_variant.clone();
        let resolved = config
            .resolve_model(model, variant.as_deref())
            .or_else(|_| config.resolve_model(model, None));
        let variant = match resolved {
            Ok(model) => model.selected_variant,
            Err(error) => {
                self.notices.push(error.to_string());
                return;
            }
        };
        match self
            .active_session
            .configure(Some(model.into()), Some(variant), None)
            .await
        {
            Ok(()) => self.refresh_view(),
            Err(error) => self.notices.push(error),
        }
    }

    pub async fn open_variants_modal(&mut self) {
        let session = &self.view.session;
        let mut items = Vec::new();
        if let Ok(model) = self
            .session_mgr
            .app_config()
            .resolve_model(&session.current_model, None)
            && let Some(variants) = model.model_config.variants
            && !variants.is_empty()
        {
            items.push(VariantItem {
                id: None,
                display_name: "default".into(),
                is_default: true,
            });
            items.extend(
                variants
                    .into_iter()
                    .filter(|id| id != "default")
                    .map(|id| VariantItem {
                        display_name: id.clone(),
                        id: Some(id),
                        is_default: false,
                    }),
            );
        }
        self.modal = ActiveModal::Variants(VariantsModalState::new(
            session.current_model.clone(),
            session.current_variant.clone(),
            items,
        ));
    }

    pub async fn switch_variant(&mut self, variant: Option<&str>) {
        let variant = variant
            .filter(|v| !matches!(*v, "" | "default"))
            .map(str::to_string);
        match self
            .active_session
            .configure(None, Some(variant.clone()), None)
            .await
        {
            Ok(()) => self.refresh_view(),
            Err(error) => self.notices.push(error),
        }
    }

    pub async fn open_config_modal(&mut self) {
        let session = &self.view.session;
        self.modal = ActiveModal::Config(ConfigModalState::new(
            session.current_model.clone(),
            session.current_variant.clone(),
            session.web_search,
            self.thinking_concise,
        ));
    }
}
