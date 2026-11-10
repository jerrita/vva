use super::*;

/// 只管理会话句柄；Session 的修改与存储由对应任务顺序执行。
pub struct SessionManager {
    store: Arc<dyn SessionStore>,
    pub(super) sessions: Mutex<Option<HashMap<String, Arc<ManagedSession>>>>,
}

impl SessionManager {
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            sessions: Mutex::new(Some(HashMap::new())),
        }
    }

    /// 保证 `session.header.id` 在存储中唯一（冲突时重新生成，最多重试 8 次）。
    pub async fn unique_session_id(&self, session: &mut Session) -> Result<(), String> {
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

    pub(super) fn spawn(
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
    pub async fn spawn_registered(
        self: &Arc<Self>,
        assembly: crate::harness::AgentAssembly,
        persisted: bool,
    ) -> Result<Arc<ManagedSession>, String> {
        let mut sessions = self.sessions.lock().await;
        let sessions = sessions.as_mut().ok_or("Session manager is shut down")?;
        let handle = self.spawn(assembly, persisted);
        sessions.insert(handle.id.clone(), handle.clone());
        Ok(handle)
    }

    /// 内存中已登记的句柄（不触发存储加载）
    pub async fn live(&self, id: &str) -> Option<Arc<ManagedSession>> {
        self.sessions
            .lock()
            .await
            .as_ref()
            .and_then(|sessions| sessions.get(id).cloned())
    }

    /// 句柄表读取：内存未命中时从存储加载（由宿主明确提供装配）
    pub async fn open(
        self: &Arc<Self>,
        id: &str,
        setup: Arc<dyn crate::harness::SessionSetup>,
    ) -> Result<Arc<ManagedSession>, String> {
        let mut guard = self.sessions.lock().await;
        let sessions = guard.as_mut().ok_or("Session manager is shut down")?;
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
        self.spawn_registered(assembly, false).await
    }

    pub async fn delete_session(&self, handle: &Arc<ManagedSession>) -> Result<(), String> {
        handle.delete().await?;
        self.sessions
            .lock()
            .await
            .as_mut()
            .ok_or("Session manager is shut down")?
            .remove(&handle.id);
        self.delete_child_sessions(&handle.id).await?;
        Ok(())
    }

    /// 级联删除 parent 会话的全部 subagent 子会话（派生链递归）：
    /// 活跃句柄经 Stop{delete} 停任务并删盘，冷会话直接删存储记录
    async fn delete_child_sessions(&self, parent_id: &str) -> Result<(), String> {
        let children: Vec<String> = self
            .store
            .list_sessions()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|session| session.parent.as_deref() == Some(parent_id))
            .map(|session| session.id)
            .collect();
        for child_id in children {
            let live = self.live(&child_id).await;
            match live {
                Some(handle) => {
                    handle.delete().await?;
                    self.sessions
                        .lock()
                        .await
                        .as_mut()
                        .ok_or("Session manager is shut down")?
                        .remove(&child_id);
                }
                None => self
                    .store
                    .delete_session(&child_id)
                    .await
                    .map_err(|error| error.to_string())?,
            }
            Box::pin(self.delete_child_sessions(&child_id)).await?;
        }
        Ok(())
    }

    /// 关闭任务并等待资源回收，保留持久化历史。
    pub async fn close(&self, id: &str) -> Result<(), String> {
        let handle = self
            .live(id)
            .await
            .ok_or_else(|| format!("Session is not open: {id}"))?;
        let result = handle.close().await;
        self.sessions
            .lock()
            .await
            .as_mut()
            .ok_or("Session manager is shut down")?
            .remove(id);
        result
    }
    /// 禁止后续登记，逐个回收全部任务；单项失败仍继续清理其余任务。
    pub async fn shutdown(&self) -> Result<(), String> {
        let sessions = self
            .sessions
            .lock()
            .await
            .take()
            .ok_or("Session manager is shut down")?;
        let mut errors = Vec::new();
        for (id, handle) in sessions {
            if let Err(error) = handle.close().await {
                errors.push(format!("{id}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
