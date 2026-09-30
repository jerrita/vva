use super::*;

fn resolve_session_editor(
    model_config: &crate::config::ModelConfig,
    settings_editor: Option<crate::model::EditorKind>,
) -> crate::model::EditorKind {
    model_config
        .editor()
        .or(settings_editor)
        .unwrap_or_default()
}

/// 只管理会话句柄；Session 的修改与存储由对应任务顺序执行。
pub struct SessionManager {
    store: Arc<dyn SessionStore>,
    /// 启动时加载的合并配置；Settings 保存后可整体热替换。
    /// 已运行的会话任务持有创建时的 Arc 快照，不受替换影响。
    config: RwLock<Arc<AppConfig>>,
    /// 启动时的自定义配置路径（CLI `--config`），热重载按相同寻址规则重新合并
    config_path: Option<PathBuf>,
    workspace: PathBuf,
    state_path: PathBuf,
    sessions: Mutex<HashMap<String, Arc<ManagedSession>>>,
    /// 前台子代理登记：父会话 id → 子会话 id（父会话被中止时联动中止）
    foreground_children: Mutex<HashMap<String, Vec<String>>>,
}

impl SessionManager {
    pub fn new(
        store: Arc<dyn SessionStore>,
        config: AppConfig,
        workspace: impl Into<PathBuf>,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            config: RwLock::new(Arc::new(config)),
            config_path: None,
            workspace: workspace.into(),
            state_path: state_path.into(),
            sessions: Mutex::new(HashMap::new()),
            foreground_children: Mutex::new(HashMap::new()),
        }
    }

    /// 记录启动时的自定义配置路径，供 reload_config 复用相同寻址
    pub fn with_config_path(mut self, path: Option<&Path>) -> Self {
        self.config_path = path.map(crate::config::expand_tilde);
        self
    }

    pub fn store(&self) -> &Arc<dyn SessionStore> {
        &self.store
    }
    pub fn app_config(&self) -> Arc<AppConfig> {
        self.config.read().expect("config lock poisoned").clone()
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// 重新加载合并配置（全局 + 项目），供 Settings 保存后热应用
    pub fn reload_config(&self) -> Result<(), String> {
        let config = crate::config::load_config_with_workspace(
            self.config_path.as_deref(),
            Some(&self.workspace),
        )
        .map_err(|e| e.to_string())?;
        *self.config.write().expect("config lock poisoned") = Arc::new(config);
        Ok(())
    }

    /// 保证 `session.header.id` 在存储中唯一（冲突时重新生成，最多重试 8 次）。
    async fn unique_session_id(&self, session: &mut Session) -> Result<(), String> {
        for _ in 0..8 {
            if self
                .store
                .load_session(&session.header.id)
                .await
                .map_err(|e| e.to_string())?
                .is_none()
            {
                return Ok(());
            }
            session.header.id = crate::model::generate_session_id();
        }
        Ok(())
    }

    fn spawn(self: &Arc<Self>, session: Session, persisted: bool) -> Arc<ManagedSession> {
        let id = session.header.id.clone();
        let (commands, rx) = mpsc::channel(16);
        let (task, view, turns) = SessionTask::new(
            session,
            persisted,
            TaskDeps {
                store: self.store.clone(),
                config: self.app_config(),
                state_path: self.state_path.clone(),
                broker: Arc::new(SessionBrokerHandle::new(self.clone())),
            },
            commands.downgrade(),
        );
        tokio::spawn(task.run(rx));
        Arc::new(ManagedSession {
            id,
            commands,
            view,
            turns,
        })
    }

    /// 启动会话任务并登记句柄
    async fn spawn_registered(
        self: &Arc<Self>,
        session: Session,
        persisted: bool,
    ) -> Arc<ManagedSession> {
        let handle = self.spawn(session, persisted);
        self.sessions
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        handle
    }

    pub async fn get_or_load(self: &Arc<Self>, id: &str) -> Result<Arc<ManagedSession>, String> {
        let handle = self.live_or_load(id).await?;
        self.remember_session(&handle.snapshot().session);
        Ok(handle)
    }

    /// 句柄表读取：内存未命中时从存储加载（不回写运行时偏好，供 broker 编排路径使用）
    async fn live_or_load(self: &Arc<Self>, id: &str) -> Result<Arc<ManagedSession>, String> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(id) {
            return Ok(session.clone());
        }
        let session = self
            .store
            .load_session(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Session not found: {id}"))?;
        let handle = self.spawn(session, true);
        sessions.insert(id.into(), handle.clone());
        Ok(handle)
    }

    pub async fn create_session(
        self: &Arc<Self>,
        model: String,
        variant: Option<String>,
        title: Option<String>,
        search: bool,
    ) -> Result<Arc<ManagedSession>, String> {
        self.create_session_in(self.workspace.clone(), model, variant, title, search)
            .await
    }

    /// 在指定工作空间目录创建会话（Web 界面按项目创建时使用）
    pub async fn create_session_in(
        self: &Arc<Self>,
        workspace: impl Into<PathBuf>,
        model: String,
        variant: Option<String>,
        title: Option<String>,
        search: bool,
    ) -> Result<Arc<ManagedSession>, String> {
        let workspace = workspace.into();
        // 新建项目语义：目录不存在时自动创建
        if !workspace.exists() {
            std::fs::create_dir_all(&workspace).map_err(|e| format!("创建项目目录失败: {e}"))?;
        }
        let config = self.app_config();
        let resolved = config
            .resolve_model(&model, variant.as_deref())
            .map_err(|e| e.to_string())?;
        let mut session =
            Session::with_model(&workspace, title, resolved.full_spec, variant, search);
        session.header.editor =
            resolve_session_editor(&resolved.model_config, config.settings.editor);
        self.unique_session_id(&mut session).await?;
        self.remember_session(&session);
        Ok(self.spawn_registered(session, false).await)
    }

    pub async fn open_session(
        self: &Arc<Self>,
        id: Option<&str>,
        model: Option<String>,
        variant: Option<String>,
        search: Option<bool>,
    ) -> Result<Arc<ManagedSession>, String> {
        match id {
            Some(id) => {
                let handle = self.get_or_load(id).await?;
                handle.configure(model, variant.map(Some), search).await?;
                Ok(handle)
            }
            None => {
                let (model, variant) = self.new_session_model(model, variant);
                self.create_session(model, variant, None, search.unwrap_or(true))
                    .await
            }
        }
    }

    fn new_session_model(
        &self,
        model: Option<String>,
        variant: Option<String>,
    ) -> (String, Option<String>) {
        let config = self.app_config();
        if let Some(model) = model {
            return (model, variant);
        }
        let state = AppState::load(&self.state_path);
        if !state.last_model.is_empty() {
            let inherited = variant.clone().or(state.last_variant.clone());
            if config
                .resolve_model(&state.last_model, inherited.as_deref())
                .is_ok()
            {
                return (state.last_model, inherited);
            }
            if config.resolve_model(&state.last_model, None).is_ok() {
                return (state.last_model, variant);
            }
        }
        (config.settings.default.clone(), variant)
    }

    fn remember_session(&self, session: &Session) {
        AppState::persist_selection(
            &self.state_path,
            &session.header.current_model,
            session.header.current_variant.as_deref(),
        );
    }

    pub async fn delete_session(&self, handle: &Arc<ManagedSession>) -> Result<(), String> {
        handle.delete().await?;
        self.sessions.lock().await.remove(&handle.id);
        self.delete_child_sessions(&handle.id).await;
        Ok(())
    }

    /// 级联删除 parent 会话的全部子会话（subagent 派生链递归）：
    /// 活跃句柄经 Stop{delete} 停任务并删盘，冷会话直接删存储记录
    async fn delete_child_sessions(&self, parent_id: &str) {
        let children: Vec<String> = match self.store.list_sessions().await {
            Ok(sessions) => sessions
                .into_iter()
                .filter(|session| session.parent.as_deref() == Some(parent_id))
                .map(|session| session.id)
                .collect(),
            Err(error) => {
                tracing::warn!("Child sessions of {parent_id} unavailable: {error}");
                Vec::new()
            }
        };
        for child_id in children {
            let live = self.sessions.lock().await.get(&child_id).cloned();
            match live {
                Some(handle) => {
                    if handle.delete().await.is_ok() {
                        self.sessions.lock().await.remove(&child_id);
                    } else {
                        // 任务已退出：通道不可达，直接删除存储记录
                        let _ = self.store.delete_session(&child_id).await;
                    }
                }
                None => {
                    let _ = self.store.delete_session(&child_id).await;
                }
            }
            Box::pin(self.delete_child_sessions(&child_id)).await;
        }
    }

    /// 内存句柄中非空闲会话的运行状态文案（与 Web 快照 `state` 同词）。
    pub async fn live_run_states(&self) -> HashMap<String, String> {
        let sessions = self.sessions.lock().await;
        sessions
            .iter()
            .filter_map(|(id, handle)| {
                let status = handle.snapshot().state.status();
                (!status.is_empty()).then(|| (id.clone(), status))
            })
            .collect()
    }

    pub async fn submit_prompt(
        &self,
        handle: Arc<ManagedSession>,
        prompt: &str,
    ) -> Result<(), String> {
        handle.submit(prompt, Vec::new(), None).await
    }

    /// 从指定用户轮次处分叉：新会话持有该轮次之前的历史，原会话不变。
    pub async fn fork_session(
        self: &Arc<Self>,
        source: &ManagedSession,
        turn_index: usize,
        title: String,
    ) -> Result<Arc<ManagedSession>, String> {
        if source.is_running() {
            return Err(super::BUSY.into());
        }
        let origin = source.snapshot().session.clone();
        if turn_index >= origin.turns.len() {
            return Err("Turn not found".into());
        }
        if origin.turns[turn_index].role != Role::User {
            return Err("Can only fork from a user message".into());
        }
        let title = title.trim();
        let title = if title.is_empty() {
            crate::model::provisional_title(&origin.turns[turn_index].text_content())
        } else {
            title.to_string()
        };
        let mut session = Session::with_model(
            origin.header.workspace_root.clone(),
            Some(title),
            origin.header.current_model.clone(),
            origin.header.current_variant.clone(),
            origin.header.web_search,
        );
        session.header.editor = origin.header.editor;
        self.unique_session_id(&mut session).await?;
        session.turns = origin.turns[..turn_index].to_vec();
        session.updated_at = chrono::Utc::now();
        self.store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;
        self.remember_session(&session);
        Ok(self.spawn_registered(session, true).await)
    }
}

/// tools 层 SessionBroker 的 harness 实现：派生子代理会话与跨会话投递。
pub(super) struct SessionBrokerHandle {
    manager: std::sync::Weak<SessionManager>,
    /// 每个调用会话独立保存投递回执；超时后可再次等待。
    receipts: Mutex<HashMap<String, std::collections::VecDeque<DeliveryReceipt>>>,
}

impl SessionBrokerHandle {
    pub(super) fn new(manager: Arc<SessionManager>) -> Self {
        Self {
            manager: Arc::downgrade(&manager),
            receipts: Mutex::new(HashMap::new()),
        }
    }

    fn manager(&self) -> Result<Arc<SessionManager>, String> {
        self.manager
            .upgrade()
            .ok_or_else(|| "Session manager is closed".into())
    }

    async fn release_foreground_child(&self, child_id: &str) {
        if let Some(manager) = self.manager.upgrade() {
            let mut children = manager.foreground_children.lock().await;
            for list in children.values_mut() {
                list.retain(|id| id != child_id);
            }
            children.retain(|_, list| !list.is_empty());
        }
    }
}

#[async_trait::async_trait]
impl crate::tools::agent::SessionBroker for SessionBrokerHandle {
    async fn spawn_subagent(
        &self,
        parent: &crate::tools::agent::SubagentContext,
        def: &crate::agents::AgentDef,
        prompt: &str,
        description: &str,
        background: bool,
    ) -> Result<crate::tools::agent::SubagentLaunch, String> {
        let manager = self.manager()?;
        if parent.depth >= crate::agents::MAX_SUBAGENT_DEPTH {
            return Err(format!(
                "Subagent nesting depth {} reached; maximum is {}",
                crate::agents::MAX_SUBAGENT_DEPTH,
                crate::agents::MAX_SUBAGENT_DEPTH
            ));
        }
        let model = def
            .model
            .clone()
            .unwrap_or_else(|| parent.current_model.clone());
        let config = manager.app_config();
        let resolved = config
            .resolve_model(&model, parent.current_variant.as_deref())
            .map_err(|e| e.to_string())?;
        let mut session = Session::with_model(
            &parent.workspace_root,
            Some(format!("[{}] {}", def.name, description)),
            resolved.full_spec,
            parent.current_variant.clone(),
            parent.web_search,
        );
        session.header.editor =
            resolve_session_editor(&resolved.model_config, config.settings.editor);
        session.header.parent = Some(parent.parent_id.clone());
        session.header.agent = Some(def.name.clone());
        session.header.depth = parent.depth + 1;
        manager.unique_session_id(&mut session).await?;
        let handle = manager.spawn_registered(session, false).await;
        let turn_seq = handle.turn_seq();
        // 后台回投 watcher 需在提交前建立订阅基线，避免漏掉首轮完成
        let background_watcher = background.then(|| {
            let mut turns = handle.turn_receiver();
            let child_id = handle.id.clone();
            let parent_id = parent.parent_id.clone();
            let manager = Arc::downgrade(&manager);
            tokio::spawn(async move {
                // 仅回投首轮完成；后续轮次由 SendMessage/WaitSession 显式交互
                if turns.changed().await.is_ok()
                    && let Some(manager) = manager.upgrade()
                {
                    let parent = manager.sessions.lock().await.get(&parent_id).cloned();
                    if let Some(parent) = parent {
                        parent.notify_subagent_finished(child_id);
                    }
                }
            })
        });
        if !background {
            manager
                .foreground_children
                .lock()
                .await
                .entry(parent.parent_id.clone())
                .or_default()
                .push(handle.id.clone());
        }
        if let Err(error) = handle.submit(prompt, Vec::new(), None).await {
            // 启动失败：回收空会话句柄与登记（任务随通道关闭退出）
            manager.sessions.lock().await.remove(&handle.id);
            self.release_foreground_child(&handle.id).await;
            if let Some(watcher) = background_watcher {
                watcher.abort();
            }
            return Err(format!("Subagent session failed to start: {error}"));
        }
        Ok(crate::tools::agent::SubagentLaunch {
            session_id: handle.id.clone(),
            turn_seq,
        })
    }

    async fn deliver(&self, session_id: &str, text: &str) -> Result<String, String> {
        let handle = self.manager()?.live_or_load(session_id).await?;
        let running = handle.is_running();
        let receipt = handle.deliver(text).await?;
        self.receipts
            .lock()
            .await
            .entry(session_id.into())
            .or_default()
            .push_back(receipt);
        Ok(if running {
            format!("Queued; session {session_id} is running and will process the message next.")
        } else {
            format!("Delivered; session {session_id} is processing the message.")
        })
    }

    async fn wait_turn(
        &self,
        session_id: &str,
        seen_seq: u64,
        timeout: Option<std::time::Duration>,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        async fn wait_timeout<F: Future>(
            timeout: Option<std::time::Duration>,
            fut: F,
        ) -> Result<F::Output, tokio::time::error::Elapsed> {
            match timeout {
                Some(duration) => tokio::time::timeout(duration, fut).await,
                None => Ok(fut.await),
            }
        }
        let receipt = self
            .receipts
            .lock()
            .await
            .get_mut(session_id)
            .and_then(|queue| queue.pop_front());
        if let Some(mut receipt) = receipt {
            let result = match wait_timeout(timeout, &mut receipt).await {
                Ok(result) => result,
                Err(_) => {
                    self.receipts
                        .lock()
                        .await
                        .entry(session_id.into())
                        .or_default()
                        .push_front(receipt);
                    return Err(format!("Timed out waiting for session {session_id}"));
                }
            };
            return result.map_err(|_| format!("Session {session_id} is closed"))?;
        }
        let handle = self.manager()?.live_or_load(session_id).await?;
        let mut turns = handle.turn_receiver();
        loop {
            if turns.borrow_and_update().seq > seen_seq {
                break;
            }
            let changed = turns.changed();
            wait_timeout(timeout, changed)
                .await
                .map_err(|_| format!("Timed out waiting for session {session_id}"))?
                .map_err(|_| format!("Session {session_id} is closed"))?;
        }
        self.release_foreground_child(session_id).await;
        Ok(handle.report())
    }

    async fn session_report(
        &self,
        session_id: &str,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        let handle = self.manager()?.live_or_load(session_id).await?;
        Ok(handle.report())
    }

    async fn abort_session(&self, session_id: &str) -> Result<(), String> {
        let handle = self
            .manager()?
            .sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| format!("Session {session_id} not found"))?;
        if !handle.is_running() {
            return Err(format!("Session {session_id} is not running"));
        }
        handle.abort_current().await
    }

    async fn abort_children(&self, parent_id: &str) {
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let children = manager
            .foreground_children
            .lock()
            .await
            .remove(parent_id)
            .unwrap_or_default();
        for child in children {
            let handle = manager.sessions.lock().await.get(&child).cloned();
            if let Some(handle) = handle
                && handle.is_running()
            {
                let _ = handle.abort_current().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::jsonl::JsonlSessionStore;
    use crate::tools::agent::{SessionBroker, SubagentReport};

    fn manager() -> (Arc<SessionManager>, PathBuf) {
        let root = std::env::temp_dir().join(format!("vva-broker-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(JsonlSessionStore::new(&root).unwrap());
        let manager = Arc::new(SessionManager::new(
            store,
            AppConfig::default(),
            &root,
            root.join("state.json"),
        ));
        (manager, root)
    }

    #[tokio::test]
    async fn releasing_manager_closes_session_tasks() {
        let (manager, root) = manager();
        let handle = manager.spawn(Session::new(&root, None), false);
        let mut turns = handle.turn_receiver();
        manager
            .sessions
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        let weak = Arc::downgrade(&manager);
        drop(handle);
        drop(manager);
        assert!(weak.upgrade().is_none());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), turns.changed())
                .await
                .unwrap()
                .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn delivery_receipts_are_isolated_and_survive_timeout_and_later_turns() {
        let (manager, root) = manager();
        let session = Session::new(&root, None);
        let id = session.header.id.clone();
        let (commands, mut rx) = mpsc::channel(4);
        let mut running = SessionView::new(session, false);
        running.state = RunState::Thinking;
        let (_, view) = watch::channel(Arc::new(running));
        let (turn_tx, turns) = watch::channel(CompletedTurn::default());
        manager.sessions.lock().await.insert(
            id.clone(),
            Arc::new(ManagedSession {
                id: id.clone(),
                commands,
                view,
                turns,
            }),
        );
        let first = SessionBrokerHandle::new(manager.clone());
        let second = SessionBrokerHandle::new(manager.clone());
        let (pending_tx, mut pending_rx) = mpsc::channel(2);
        let actor = tokio::spawn(async move {
            for _ in 0..2 {
                let Some(Command::Deliver { delivery, reply }) = rx.recv().await else {
                    panic!("expected delivery");
                };
                let _ = reply.send(Ok(()));
                pending_tx.send(delivery).await.unwrap();
            }
        });
        first.deliver(&id, "first request").await.unwrap();
        second.deliver(&id, "second request").await.unwrap();
        let a = pending_rx.recv().await.unwrap();
        let b = pending_rx.recv().await.unwrap();
        // 正在执行的旧轮完成，不能解除对排队投递的等待。
        turn_tx.send_replace(CompletedTurn {
            seq: 1,
            text: "earlier result".into(),
            error: None,
        });
        assert!(first.wait_turn(&id, 0, Some(Duration::ZERO)).await.is_err());
        for (delivery, text) in [(a, "first reply"), (b, "second reply")] {
            assert!(
                delivery
                    .completion
                    .send(Ok(SubagentReport {
                        session_id: id.clone(),
                        agent: String::new(),
                        title: String::new(),
                        turn_seq: 2,
                        text: text.into(),
                        error: None,
                    }))
                    .is_ok()
            );
        }
        // 后续轮次覆盖 watch 后，回执仍返回对应投递的固定结果。
        turn_tx.send_replace(CompletedTurn {
            seq: 3,
            text: "later result".into(),
            error: None,
        });
        assert_eq!(
            second
                .wait_turn(&id, 3, Some(Duration::from_secs(1)))
                .await
                .unwrap()
                .text,
            "second reply"
        );
        assert_eq!(
            first
                .wait_turn(&id, 3, Some(Duration::from_secs(1)))
                .await
                .unwrap()
                .text,
            "first reply"
        );
        actor.await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
