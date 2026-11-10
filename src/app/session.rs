use super::setup::{SessionPreamble, SessionToolFactory, VvaSessionSetup};
use crate::config::{AppConfig, AppState};
use crate::harness::session::handle::Command;
use crate::harness::{AgentAssembly, ManagedSession, SessionManager};
use crate::model::{Role, Session, SessionToggles, ToggleOverrides};
use crate::storage::SessionStore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

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
pub struct VvaSessionManager {
    core: Arc<SessionManager>,
    /// 启动时加载的合并配置；Settings 保存后可整体热替换。
    /// 已运行的会话任务持有创建时的 Arc 快照，不受替换影响。
    config: RwLock<Arc<AppConfig>>,
    /// 启动时的自定义配置路径（CLI `--config`），热重载按相同寻址规则重新合并
    config_path: Option<PathBuf>,
    workspace: PathBuf,
    state_path: PathBuf,
    /// 前台子代理登记：父会话 id → 子会话 id（父会话被中止时联动中止）
    foreground_children: Mutex<HashMap<String, Vec<String>>>,
    /// 会话工具工厂：按会话 header 追加运行期工具；缺省不追加
    tool_factory: Option<SessionToolFactory>,
    /// 会话前导：首次落盘时追加的 reminder 提供者；缺省不追加
    preamble: Option<SessionPreamble>,
}

impl VvaSessionManager {
    pub fn new(
        store: Arc<dyn SessionStore>,
        config: AppConfig,
        workspace: impl Into<PathBuf>,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            core: Arc::new(SessionManager::new(store)),
            config: RwLock::new(Arc::new(config)),
            config_path: None,
            workspace: workspace.into(),
            state_path: state_path.into(),
            foreground_children: Mutex::new(HashMap::new()),
            tool_factory: None,
            preamble: None,
        }
    }

    /// 安装会话工具工厂（须在包装为 Arc 之前调用）
    pub fn with_tool_factory(mut self, factory: SessionToolFactory) -> Self {
        self.tool_factory = Some(factory);
        self
    }

    /// 安装会话前导提供者（须在包装为 Arc 之前调用）
    pub fn with_preamble(mut self, preamble: SessionPreamble) -> Self {
        self.preamble = Some(preamble);
        self
    }

    /// 记录启动时的自定义配置路径，供 reload_config 复用相同寻址
    pub fn with_config_path(mut self, path: Option<&Path>) -> Self {
        self.config_path = path.map(crate::config::expand_tilde);
        self
    }

    pub fn store(&self) -> &Arc<dyn SessionStore> {
        self.core.store()
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

    fn setup(self: &Arc<Self>, session: &Session) -> Arc<VvaSessionSetup> {
        let setup = VvaSessionSetup::new(
            self.app_config(),
            self.store().clone(),
            self.state_path.clone(),
            Arc::new(SessionBrokerHandle::new(
                self.clone(),
                session.header.id.clone(),
            )),
            self.tool_factory.clone(),
            self.preamble.clone(),
        );
        Arc::new(if session.header.agent_managed() {
            crate::space::session_setup(setup)
        } else {
            setup
        })
    }

    #[cfg(test)]
    fn spawn(self: &Arc<Self>, session: Session, persisted: bool) -> Arc<ManagedSession> {
        let setup = self.setup(&session);
        self.core.spawn(AgentAssembly { session, setup }, persisted)
    }

    async fn spawn_registered(
        self: &Arc<Self>,
        session: Session,
        persisted: bool,
    ) -> Arc<ManagedSession> {
        let setup = self.setup(&session);
        self.core
            .spawn_registered(AgentAssembly { session, setup }, persisted)
            .await
    }

    pub async fn get_or_load(self: &Arc<Self>, id: &str) -> Result<Arc<ManagedSession>, String> {
        let handle = self.live_or_load(id).await?;
        self.remember_session(&handle.snapshot().session);
        Ok(handle)
    }

    /// 内存中已登记的句柄（不触发存储加载）
    pub async fn live(&self, id: &str) -> Option<Arc<ManagedSession>> {
        self.sessions.lock().await.get(id).cloned()
    }

    pub async fn live_or_load(self: &Arc<Self>, id: &str) -> Result<Arc<ManagedSession>, String> {
        if let Some(handle) = self.core.live(id).await {
            return Ok(handle);
        }
        let session = self
            .store()
            .load_session(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Session not found: {id}"))?;
        self.core.open(id, self.setup(&session)).await
    }

    pub async fn create_session(
        self: &Arc<Self>,
        model: String,
        variant: Option<String>,
        title: Option<String>,
        toggles: SessionToggles,
    ) -> Result<Arc<ManagedSession>, String> {
        self.create_session_in(self.workspace.clone(), model, variant, title, toggles)
            .await
    }

    /// 在指定工作空间目录创建会话（Web 界面按项目创建时使用）
    pub async fn create_session_in(
        self: &Arc<Self>,
        workspace: impl Into<PathBuf>,
        model: String,
        variant: Option<String>,
        title: Option<String>,
        toggles: SessionToggles,
    ) -> Result<Arc<ManagedSession>, String> {
        self.create_session_with(workspace, model, variant, title, toggles, |_| {})
            .await
    }

    /// 创建会话并在登记前由 `init` 调整 header（agent space 设置 kind / agent / origin）
    pub async fn create_session_with(
        self: &Arc<Self>,
        workspace: impl Into<PathBuf>,
        model: String,
        variant: Option<String>,
        title: Option<String>,
        toggles: SessionToggles,
        init: impl FnOnce(&mut crate::model::SessionHeader),
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
            Session::with_model(&workspace, title, resolved.full_spec, variant, toggles);
        session.header.editor =
            resolve_session_editor(&resolved.model_config, config.settings.editor);
        init(&mut session.header);
        self.unique_session_id(&mut session).await?;
        self.remember_session(&session);
        Ok(self.spawn_registered(session, false).await)
    }

    /// 接管一个已构造完毕且带历史轮次的会话（rollover 新会话）：落盘后登记，
    /// 不回写运行时模型偏好
    pub async fn adopt_session(
        self: &Arc<Self>,
        mut session: Session,
    ) -> Result<Arc<ManagedSession>, String> {
        self.unique_session_id(&mut session).await?;
        self.store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;
        Ok(self.spawn_registered(session, true).await)
    }

    pub async fn open_session(
        self: &Arc<Self>,
        id: Option<&str>,
        model: Option<String>,
        variant: Option<String>,
        toggles: ToggleOverrides,
    ) -> Result<Arc<ManagedSession>, String> {
        match id {
            Some(id) => {
                let handle = self.get_or_load(id).await?;
                handle.configure(model, variant.map(Some), toggles).await?;
                Ok(handle)
            }
            None => {
                let (model, variant) = self.new_session_model(model, variant);
                self.create_session(
                    model,
                    variant,
                    None,
                    toggles.apply(SessionToggles::default()),
                )
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

    /// 从指定用户轮次处分叉：新会话持有该轮次之前的历史，原会话不变。
    pub async fn fork_session(
        self: &Arc<Self>,
        source: &ManagedSession,
        turn_index: usize,
        title: String,
    ) -> Result<Arc<ManagedSession>, String> {
        if source.is_running() {
            return Err(crate::harness::session::BUSY.into());
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
            origin.toggles(),
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
pub(crate) struct SessionBrokerHandle {
    manager: std::sync::Weak<VvaSessionManager>,
    owner_id: String,
}

impl SessionBrokerHandle {
    pub(crate) fn new(manager: Arc<VvaSessionManager>, owner_id: String) -> Self {
        Self {
            manager: Arc::downgrade(&manager),
            owner_id,
        }
    }

    async fn watch_session(
        &self,
        session_id: String,
        receipt: crate::harness::session::handle::DeliveryReceipt,
        background: bool,
    ) -> Result<(), String> {
        let owner = self
            .manager()?
            .live(&self.owner_id)
            .await
            .ok_or_else(|| "Calling session is closed".to_string())?;
        owner
            .request(|reply| Command::WatchSession {
                session_id,
                receipt,
                background,
                reply,
            })
            .await
    }

    fn manager(&self) -> Result<Arc<VvaSessionManager>, String> {
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
            SessionToggles {
                web_search: parent.web_search,
            },
        );
        session.header.editor =
            resolve_session_editor(&resolved.model_config, config.settings.editor);
        session.header.parent = Some(parent.parent_id.clone());
        session.header.agent = Some(def.name.clone());
        session.header.depth = parent.depth + 1;
        manager.unique_session_id(&mut session).await?;
        let handle = manager.spawn_registered(session, false).await;
        let turn_seq = handle.turn_seq();
        if !background {
            manager
                .foreground_children
                .lock()
                .await
                .entry(parent.parent_id.clone())
                .or_default()
                .push(handle.id.clone());
        }
        let submitted = if background {
            handle.deliver(prompt).await.map(Some)
        } else {
            handle.submit(prompt, Vec::new(), None).await.map(|()| None)
        };
        let receipt = match submitted {
            Ok(receipt) => receipt,
            Err(error) => {
                // 启动失败：回收空会话句柄与登记（任务随通道关闭退出）
                manager.sessions.lock().await.remove(&handle.id);
                self.release_foreground_child(&handle.id).await;
                return Err(format!("Subagent session failed to start: {error}"));
            }
        };
        if let Some(receipt) = receipt {
            self.watch_session(handle.id.clone(), receipt, true).await?;
        }
        Ok(crate::tools::agent::SubagentLaunch {
            session_id: handle.id.clone(),
            turn_seq,
        })
    }

    async fn deliver(&self, session_id: &str, text: &str) -> Result<String, String> {
        if session_id == self.owner_id {
            return Err("SendMessage requires another session".into());
        }
        let handle = self.manager()?.live_or_load(session_id).await?;
        let running = handle.is_running();
        let receipt = handle.deliver(text).await?;
        self.watch_session(session_id.to_string(), receipt, false)
            .await?;
        Ok(if running {
            format!(
                "Queued; session {session_id} is running and will process the message next. Its reply will arrive automatically."
            )
        } else {
            format!(
                "Delivered; session {session_id} is processing the message. Its reply will arrive automatically."
            )
        })
    }

    async fn wait_turn(
        &self,
        session_id: &str,
        seen_seq: u64,
        timeout: Option<std::time::Duration>,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        let handle = self.manager()?.live_or_load(session_id).await?;
        let report = match timeout {
            Some(duration) => tokio::time::timeout(duration, handle.settled_report(seen_seq))
                .await
                .map_err(|_| format!("Timed out waiting for session {session_id}"))??,
            None => handle.settled_report(seen_seq).await?,
        };
        self.release_foreground_child(session_id).await;
        Ok(report)
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
        if !handle.is_running() && !handle.has_background_work() {
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
                && (handle.is_running() || handle.has_background_work())
            {
                let _ = handle.abort_current().await;
            }
        }
    }
}

impl std::ops::Deref for VvaSessionManager {
    type Target = SessionManager;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::session::handle::CompletedTurn;
    use crate::harness::{RunState, SessionView};
    use crate::storage::jsonl::JsonlSessionStore;
    use crate::tools::agent::{SessionBroker, SubagentReport};
    use tokio::sync::{mpsc, watch};

    fn manager() -> (Arc<VvaSessionManager>, PathBuf) {
        let root = std::env::temp_dir().join(format!("vva-broker-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(JsonlSessionStore::new(&root).unwrap());
        let manager = Arc::new(VvaSessionManager::new(
            store,
            AppConfig::default(),
            &root,
            root.join("state.json"),
        ));
        (manager, root)
    }

    #[tokio::test]
    async fn cross_session_requests_keep_independent_receipts() {
        let (manager, root) = manager();
        let target = manager.spawn(Session::new(&root, None), false);
        let id = target.id.clone();
        let (commands, mut requests) = mpsc::channel(4);
        let mut target_view = SessionView::new(Session::new(&root, None), false);
        target_view.state = RunState::Thinking;
        let (_, view) = watch::channel(Arc::new(target_view));
        let (_, turns) = watch::channel(CompletedTurn::default());
        manager.sessions.lock().await.insert(
            id.clone(),
            Arc::new(ManagedSession {
                id: id.clone(),
                commands,
                view,
                turns,
            }),
        );
        let mut callers = Vec::new();
        for _ in 0..2 {
            let session = Session::new(&root, None);
            let owner_id = session.header.id.clone();
            let (commands, rx) = mpsc::channel(1);
            let (_, view) = watch::channel(Arc::new(SessionView::new(session, false)));
            let (_, turns) = watch::channel(CompletedTurn::default());
            let handle = Arc::new(ManagedSession {
                id: owner_id.clone(),
                commands,
                view,
                turns,
            });
            manager
                .sessions
                .lock()
                .await
                .insert(owner_id.clone(), handle);
            callers.push((SessionBrokerHandle::new(manager.clone(), owner_id), rx));
        }
        let actor = tokio::spawn(async move {
            let mut deliveries = Vec::new();
            for _ in 0..2 {
                let Some(Command::Deliver { delivery, reply }) = requests.recv().await else {
                    panic!("expected delivery");
                };
                let _ = reply.send(Ok(()));
                deliveries.push(delivery);
            }
            for delivery in deliveries {
                let report = SubagentReport {
                    session_id: id.clone(),
                    agent: String::new(),
                    title: String::new(),
                    turn_seq: 2,
                    error: None,
                    text: delivery.text.clone(),
                };
                let _ = delivery.completion.send(Ok(report));
            }
        });
        let mut receipts = Vec::new();
        for (index, (broker, rx)) in callers.iter_mut().enumerate() {
            let text = format!("reply {index}");
            let future = broker.deliver(&target.id, &text);
            tokio::pin!(future);
            tokio::select! {
                _ = &mut future => panic!("registration must be acknowledged"),
                command = rx.recv() => {
                    let Some(Command::WatchSession { receipt, background, reply, .. }) = command else { panic!("expected registration") };
                    assert!(!background);
                    receipts.push(receipt);
                    let _ = reply.send(Ok(()));
                }
            }
            future.await.unwrap();
        }
        for (index, receipt) in receipts.into_iter().enumerate() {
            let report = receipt.await.unwrap().unwrap();
            assert_eq!(report.text, format!("reply {index}"));
        }
        actor.await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
