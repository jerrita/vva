use super::*;

/// 只管理会话句柄；Session 的修改与存储由对应任务顺序执行。
pub struct SessionManager {
    pub(crate) store: Arc<dyn SessionStore>,
    pub(crate) sessions: Mutex<HashMap<String, Arc<ManagedSession>>>,
}

impl SessionManager {
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> &Arc<dyn SessionStore> {
        &self.store
    }

    /// 保证 `session.header.id` 在存储中唯一（冲突时重新生成，最多重试 8 次）。
    pub(crate) async fn unique_session_id(&self, session: &mut Session) -> Result<(), String> {
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

    pub(crate) fn spawn(
        self: &Arc<Self>,
        assembly: crate::harness::AgentAssembly,
        persisted: bool,
    ) -> Arc<ManagedSession> {
        let crate::harness::AgentAssembly { session, setup } = assembly;
        let id = session.header.id.clone();
        let (commands, rx) = mpsc::channel(16);
        let (task, view, turns) = SessionTask::new(
            session,
            persisted,
            TaskDeps {
                store: self.store.clone(),
                setup,
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
    pub(crate) async fn spawn_registered(
        self: &Arc<Self>,
        assembly: crate::harness::AgentAssembly,
        persisted: bool,
    ) -> Arc<ManagedSession> {
        let handle = self.spawn(assembly, persisted);
        self.sessions
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        handle
    }

    /// 内存中已登记的句柄（不触发存储加载）
    pub async fn live(&self, id: &str) -> Option<Arc<ManagedSession>> {
        self.sessions.lock().await.get(id).cloned()
    }

    /// 句柄表读取：内存未命中时从存储加载（不回写运行时偏好，供 broker 与 agent space 使用）
    pub async fn open(
        self: &Arc<Self>,
        id: &str,
        setup: Arc<dyn crate::harness::SessionSetup>,
    ) -> Result<Arc<ManagedSession>, String> {
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
        let handle = self.spawn(crate::harness::AgentAssembly { session, setup }, true);
        sessions.insert(id.into(), handle.clone());
        Ok(handle)
    }

    pub async fn create(
        self: &Arc<Self>,
        mut assembly: crate::harness::AgentAssembly,
    ) -> Result<Arc<ManagedSession>, String> {
        self.unique_session_id(&mut assembly.session).await?;
        Ok(self.spawn_registered(assembly, false).await)
    }

    pub async fn adopt(
        self: &Arc<Self>,
        mut assembly: crate::harness::AgentAssembly,
    ) -> Result<Arc<ManagedSession>, String> {
        self.unique_session_id(&mut assembly.session).await?;
        self.store
            .save_header(&assembly.session)
            .await
            .map_err(|e| e.to_string())?;
        Ok(self.spawn_registered(assembly, true).await)
    }

    pub async fn delete_session(&self, handle: &Arc<ManagedSession>) -> Result<(), String> {
        handle.delete().await?;
        self.sessions.lock().await.remove(&handle.id);
        self.delete_child_sessions(&handle.id).await;
        Ok(())
    }

    /// 级联删除 parent 会话的全部 subagent 子会话（派生链递归）：
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
}
