use super::{App, ToolExpand};
use crate::ui::history::compute_stats;
use crate::ui::modals::*;

/// 工具块无覆盖值时的默认展示档位：变更类工具直接全文，其余预览
fn default_tool_mode(full_only: bool) -> ToolExpand {
    if full_only {
        ToolExpand::Full
    } else {
        ToolExpand::Preview
    }
}

impl App {
    pub fn refresh_view(&mut self) {
        let view = self.active_session.snapshot();
        if !std::sync::Arc::ptr_eq(&view.session, &self.view.session) {
            self.stats = compute_stats(&view.session, &self.session_mgr.app_config());
        }
        self.view = view;
        if let Some(request) = &self.view.pending_user_question {
            let same = matches!(&self.modal, ActiveModal::AskUser(state) if state.request.id == request.id);
            if !same {
                self.modal = ActiveModal::AskUser(AskUserModalState::new(request.clone()));
            }
        } else if matches!(self.modal, ActiveModal::AskUser(_)) {
            self.modal = ActiveModal::None;
        }
    }

    /// 接管一个会话句柄：重置界面局部状态并订阅其视图（切换、新建共用）
    pub fn attach_session(&mut self, session: std::sync::Arc<crate::harness::ManagedSession>) {
        self.active_session = session;
        self.view_rx = self.active_session.subscribe();
        self.clear_selections();
        self.chat.lines.clear();
        self.tool_expand.clear();
        self.response_expand.clear();
        self.feedback.clear();
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
        let header = &self.view.session.header;
        let handle = self
            .session_mgr
            .create_session(
                header.current_model.clone(),
                header.current_variant.clone(),
                None,
                self.view.session.toggles(),
            )
            .await?;
        self.attach_session(handle);
        Ok(())
    }

    /// 删除当前会话并以原配置新建（/clear 命令与会话弹窗删除共用）
    pub async fn delete_and_recreate_session(&mut self) {
        if let Err(error) = self.session_mgr.delete_session(&self.active_session).await {
            self.feedback.push(error);
            return;
        }
        if let Err(error) = self.new_session().await {
            self.feedback.push(error);
        }
    }

    pub async fn open_sessions_modal(&mut self) {
        match self.session_mgr.store().list_sessions().await {
            Ok(sessions) => {
                self.modal = ActiveModal::Sessions(SessionsModalState::new(
                    sessions,
                    self.session_mgr.workspace(),
                ));
            }
            Err(error) => self.feedback.push(error.to_string()),
        }
    }

    /// 扫描全局与项目技能目录并打开 /skills 只读弹窗
    pub async fn open_skills_modal(&mut self) {
        let workspace = self.session_mgr.workspace().to_path_buf();
        let skills =
            tokio::task::spawn_blocking(move || crate::skills::discover_skills(&workspace))
                .await
                .unwrap_or_default();
        // 同步刷新补全 / 斜杠调用所用的缓存，/skills 之后立即可用最新技能
        self.skills = skills.clone();
        self.modal = ActiveModal::Skills(SkillsModalState::new(skills));
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
                    context_limit: model.limit.as_ref().map(|l| l.context),
                    variants: model.variants.clone(),
                });
            }
        }
        models.sort_by(|a, b| a.full_id.cmp(&b.full_id));
        self.modal = ActiveModal::Models(ModelsModalState::new(
            models,
            &self.view.session.header.current_model,
        ));
    }

    pub async fn switch_model(&mut self, model: &str) {
        let config = self.session_mgr.app_config();
        let variant = self.view.session.header.current_variant.clone();
        let resolved = config
            .resolve_model(model, variant.as_deref())
            .or_else(|_| config.resolve_model(model, None));
        let variant = match resolved {
            Ok(model) => model.selected_variant,
            Err(error) => {
                self.feedback.push(error.to_string());
                return;
            }
        };
        match self
            .active_session
            .configure(Some(model.into()), Some(variant), Default::default())
            .await
        {
            Ok(()) => self.refresh_view(),
            Err(error) => self.feedback.push(error),
        }
    }

    pub async fn open_variants_modal(&mut self) {
        let items = self.variant_options();
        let (model, variant) = (
            self.view.session.header.current_model.clone(),
            self.view.session.header.current_variant.clone(),
        );
        self.modal = ActiveModal::Variants(VariantsModalState::new(model, variant, items));
    }

    /// 当前模型可用的推理档位选项（default 置首，与 /variant 弹窗同源）
    fn variant_options(&self) -> Vec<VariantItem> {
        let config = self.session_mgr.app_config();
        let Ok(model) = config.resolve_model(&self.view.session.header.current_model, None) else {
            return Vec::new();
        };
        let Some(variants) = model.model_config.variants else {
            return Vec::new();
        };
        if variants.is_empty() {
            return Vec::new();
        }
        let mut items = vec![VariantItem {
            id: None,
            display_name: "default".into(),
        }];
        items.extend(
            variants
                .into_iter()
                .filter(|id| id != "default")
                .map(|id| VariantItem {
                    display_name: id.clone(),
                    id: Some(id),
                }),
        );
        items
    }

    /// 右键档位标签：按声明顺序循环切换到下一个推理档位（越过末位回到 default）
    pub async fn cycle_variant(&mut self) {
        let options = self.variant_options();
        if options.len() < 2 {
            return;
        }
        let current = active_variant_index(
            &options,
            self.view.session.header.current_variant.as_deref(),
        )
        .unwrap_or(0);
        let next = &options[(current + 1) % options.len()];
        let next_id = next.id.clone();
        self.switch_variant(next_id.as_deref()).await;
    }

    pub async fn switch_variant(&mut self, variant: Option<&str>) {
        let variant = variant
            .filter(|v| !matches!(*v, "" | "default"))
            .map(str::to_string);
        match self
            .active_session
            .configure(None, Some(variant.clone()), Default::default())
            .await
        {
            Ok(()) => self.refresh_view(),
            Err(error) => self.feedback.push(error),
        }
    }

    pub async fn open_config_modal(&mut self) {
        let session = &self.view.session;
        self.modal = ActiveModal::Config(ConfigModalState::new(
            session.header.current_model.clone(),
            session.header.current_variant.clone(),
            session.header.web_search,
            self.thinking_concise,
        ));
    }

    pub async fn answer_user(
        &mut self,
        request_id: String,
        answers: Vec<crate::model::UserAnswer>,
    ) {
        self.modal = ActiveModal::None;
        if let Err(error) = self.active_session.answer_user(request_id, answers).await {
            self.feedback.push(error);
        }
        self.refresh_view();
    }

    /// 读取工具块的生效展示档位（覆盖值优先，缺省按工具类别）
    fn tool_expand_at(&self, id: &str, full_only: bool) -> ToolExpand {
        self.tool_expand
            .get(id)
            .copied()
            .unwrap_or_else(|| default_tool_mode(full_only))
    }

    /// 单击工具标题行隐藏内容块；隐藏后再次单击恢复默认展示。Shell 预览块单击可展开全文。
    pub fn handle_tool_click(&mut self, line: usize) -> bool {
        let Some(hit) = self.chat_cache.tool_hit_at(line) else {
            return false;
        };
        let id = hit.id.clone();
        let mode = self.tool_expand_at(&id, hit.full_only);
        let next = if hit.contains_header(line) {
            Some(if mode == ToolExpand::Hidden {
                default_tool_mode(hit.full_only)
            } else {
                ToolExpand::Hidden
            })
        } else {
            (mode == ToolExpand::Preview && hit.truncated).then_some(ToolExpand::Full)
        };
        if let Some(next) = next {
            self.tool_expand.insert(id, next);
        }
        true
    }

    /// 右击展开的工具块执行收折：Edit / Write 直接隐藏；Shell 全文收为预览，预览再隐藏。
    pub fn handle_tool_right_click(&mut self, line: usize) -> bool {
        let Some(hit) = self.chat_cache.tool_hit_at(line) else {
            return false;
        };
        if !hit.contains_block(line) {
            return false;
        }
        let id = hit.id.clone();
        let next = match self.tool_expand_at(&id, hit.full_only) {
            ToolExpand::Full if !hit.full_only => Some(ToolExpand::Preview),
            ToolExpand::Full | ToolExpand::Preview => Some(ToolExpand::Hidden),
            ToolExpand::Hidden => None,
        };
        if let Some(next) = next {
            self.tool_expand.insert(id, next);
        }
        true
    }

    /// 单击思考分组标题行切换该组展开/折叠；覆盖值与全局默认一致时移除条目。
    pub fn handle_thought_click(&mut self, line: usize) -> bool {
        let Some(hit) = self.chat_cache.thought_hit_at(line) else {
            return false;
        };
        let id = hit.id.clone();
        let expanded = self
            .thought_expand
            .get(&id)
            .copied()
            .unwrap_or(!self.thinking_concise);
        if expanded == self.thinking_concise {
            self.thought_expand.remove(&id);
        } else {
            self.thought_expand.insert(id, !expanded);
        }
        true
    }

    /// 单击引用汇总标题行切换展开/折叠；默认折叠，无全局默认值参与。
    pub fn handle_citations_click(&mut self, line: usize) -> bool {
        let Some(hit) = self.chat_cache.citations_hit_at(line) else {
            return false;
        };
        let id = hit.id.clone();
        let expanded = self.citations_expand.get(&id).copied().unwrap_or(false);
        self.citations_expand.insert(id, !expanded);
        true
    }

    /// 单击收折的历史响应摘要，展开该响应。
    pub fn handle_response_click(&mut self, line: usize) -> bool {
        let Some(hit) = self.chat_cache.response_hit_at(line) else {
            return false;
        };
        if !hit.collapsed {
            return false;
        }
        self.response_expand.insert(hit.id.clone(), true);
        true
    }

    /// 右击历史响应内的组件时优先收折组件；组件已无法继续收折时收折整个响应。
    pub fn handle_response_right_click(&mut self, line: usize) -> bool {
        if self.handle_tool_right_click(line) {
            return true;
        }
        let Some(hit) = self.chat_cache.response_hit_at(line) else {
            return false;
        };
        let (id, collapsed, collapsible) = (hit.id.clone(), hit.collapsed, hit.collapsible);
        if !collapsible {
            return false;
        }
        if collapsed {
            return true;
        }
        if let Some(thought) = self.chat_cache.thought_hit_at(line)
            && thought.contains_body(line)
        {
            return self.handle_thought_click(line);
        }
        self.response_expand.insert(id, false);
        true
    }

    /// 单击历史用户消息：打开 Revert / Fork 选择
    pub fn open_message_action_at_line(&mut self, line: usize) {
        if self.active_session.is_running() {
            return;
        }
        let Some(hit) = self.chat_cache.user_hit_at(line) else {
            return;
        };
        let Some(turn) = self.view.session.turns.get(hit.turn_index) else {
            return;
        };
        if turn.role != crate::model::Role::User {
            return;
        }
        self.modal = ActiveModal::MessageAction(MessageActionState::new(
            hit.turn_index,
            turn.text_content(),
        ));
    }

    pub async fn revert_user_turn(&mut self, turn_index: usize, prompt: String) {
        self.modal = ActiveModal::None;
        match self.active_session.revert(turn_index).await {
            Ok(()) => {
                self.refresh_view();
                self.set_input(&prompt);
                self.scroll_to_bottom();
            }
            Err(error) => self.feedback.push(error),
        }
    }

    pub fn open_fork_title(&mut self, turn_index: usize, prompt: String) {
        let default = self
            .view
            .session
            .header
            .title
            .clone()
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| crate::model::provisional_title(&prompt));
        self.modal = ActiveModal::ForkTitle(ForkTitleState::new(turn_index, prompt, &default));
    }

    pub async fn fork_user_turn(&mut self, turn_index: usize, title: String) {
        self.modal = ActiveModal::None;
        match self
            .session_mgr
            .fork_session(&self.active_session, turn_index, title)
            .await
        {
            Ok(handle) => self.attach_session(handle),
            Err(error) => self.feedback.push(error),
        }
    }
}
