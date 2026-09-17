use super::*;

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

    /// 保证 `session.id` 在存储中唯一（冲突时重新生成，最多重试 8 次）。
    async fn unique_session_id(&self, session: &mut Session) -> Result<(), String> {
        for _ in 0..8 {
            if self
                .store
                .load_session(&session.id)
                .await
                .map_err(|e| e.to_string())?
                .is_none()
            {
                return Ok(());
            }
            session.id = crate::model::generate_session_id();
        }
        Ok(())
    }

    fn spawn(&self, session: Session, persisted: bool) -> Arc<ManagedSession> {
        let id = session.id.clone();
        let view = SessionView {
            revision: 0,
            session: Arc::new(session),
            draft: Vec::new(),
            notices: Vec::new(),
            state: RunState::Idle,
            persisted,
            pending_user_question: None,
        };
        let (view_tx, view_rx) = watch::channel(Arc::new(view.clone()));
        let (commands, rx) = mpsc::channel(16);
        let (ask_user_tx, ask_user_rx) = mpsc::channel(4);
        let task = SessionTask {
            view,
            view_tx,
            store: self.store.clone(),
            config: self.app_config(),
            hashline: crate::tools::hashline::HashlineSession::new(),
            state_path: self.state_path.clone(),
            commands: commands.downgrade(),
            dirty: false,
            cli: None,
            stop_request: None,
            notified: HashSet::new(),
            title_task: None,
            text_parts: HashMap::new(),
            ask_user_tx,
            ask_user_rx,
            pending_user_question: None,
        };
        tokio::spawn(task.run(rx));
        Arc::new(ManagedSession {
            id,
            commands,
            view: view_rx,
        })
    }

    pub async fn get_or_load(&self, id: &str) -> Result<Arc<ManagedSession>, String> {
        let handle = {
            let mut sessions = self.sessions.lock().await;
            if let Some(session) = sessions.get(id) {
                session.clone()
            } else {
                let session = self
                    .store
                    .load_session(id)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("Session not found: {id}"))?;
                let handle = self.spawn(session, true);
                sessions.insert(id.into(), handle.clone());
                handle
            }
        };
        self.remember_session(&handle.snapshot().session);
        Ok(handle)
    }

    pub async fn create_session(
        &self,
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
        &self,
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
        session.editor = resolved
            .model_config
            .editor()
            .or(config.settings.editor)
            .unwrap_or_default();
        self.unique_session_id(&mut session).await?;
        self.remember_session(&session);
        let handle = self.spawn(session, false);
        self.sessions
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        Ok(handle)
    }

    pub async fn open_session(
        &self,
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
            &session.current_model,
            session.current_variant.as_deref(),
        );
    }

    pub async fn delete_session(&self, handle: &Arc<ManagedSession>) -> Result<(), String> {
        handle
            .request(|reply| Command::Stop {
                delete: true,
                reply,
            })
            .await?;
        self.sessions.lock().await.remove(&handle.id);
        Ok(())
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
        &self,
        source: &ManagedSession,
        turn_index: usize,
        title: String,
    ) -> Result<Arc<ManagedSession>, String> {
        if source.is_running() {
            return Err("Session is currently running another prompt".into());
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
            origin.workspace_root.clone(),
            Some(title),
            origin.current_model.clone(),
            origin.current_variant.clone(),
            origin.web_search,
        );
        session.editor = origin.editor;
        self.unique_session_id(&mut session).await?;
        session.turns = origin.turns[..turn_index].to_vec();
        session.updated_at = chrono::Utc::now();
        self.store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;
        self.remember_session(&session);
        let handle = self.spawn(session, true);
        self.sessions
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        Ok(handle)
    }
}
