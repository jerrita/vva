use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tracing::error;

use crate::config::{AppConfig, ResolvedModel};
use crate::harness::{AgentConfig, AgentEngine, AgentEvent};
use crate::model::{Message, Session, Turn};
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::gemini::GeminiProvider;
use crate::provider::response::ResponseProvider;
use crate::provider::{CompletionOptions, Provider};
use crate::storage::SessionStore;
use crate::tools::bash::BashTool;
use crate::tools::file::{FileEditTool, FileViewTool};
use crate::tools::ToolRegistry;

/// 单个活跃会话的运行时单例句柄
pub struct ManagedSession {
    pub id: String,
    pub session: Arc<RwLock<Session>>,
    pub is_running: Arc<AtomicBool>,
    pub status_text: Arc<RwLock<String>>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    pub task_abort_handle: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
}

impl ManagedSession {
    pub fn new(session: Session) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let id = session.id.clone();
        Self {
            id,
            session: Arc::new(RwLock::new(session)),
            is_running: Arc::new(AtomicBool::new(false)),
            status_text: Arc::new(RwLock::new(String::new())),
            event_tx,
            task_abort_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    pub async fn status(&self) -> String {
        self.status_text.read().await.clone()
    }

    /// 取消当前正在执行的轮次
    pub async fn abort_current(&self) {
        let mut handle = self.task_abort_handle.lock().await;
        if let Some(h) = handle.take() {
            h.abort();
        }
        self.is_running.store(false, Ordering::SeqCst);
        *self.status_text.write().await = String::new();
        let _ = self.event_tx.send(AgentEvent::Finished);
    }
}

/// 全局 Session 实例管理器，保证 session_id 对应的单例全局唯一
pub struct SessionManager {
    store: Arc<dyn SessionStore>,
    app_config: Option<AppConfig>,
    workspace: PathBuf,
    sessions: Arc<RwLock<HashMap<String, Arc<ManagedSession>>>>,
}

impl SessionManager {
    pub fn new(
        store: Arc<dyn SessionStore>,
        app_config: Option<AppConfig>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            app_config,
            workspace: workspace.into(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn store(&self) -> Arc<dyn SessionStore> {
        self.store.clone()
    }

    pub fn app_config(&self) -> Option<&AppConfig> {
        self.app_config.as_ref()
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// 获取或从存储加载指定的 Session 单例
    pub async fn get_or_load(&self, session_id: &str) -> Result<Arc<ManagedSession>, String> {
        // 先查缓存
        {
            let map = self.sessions.read().await;
            if let Some(s) = map.get(session_id) {
                return Ok(s.clone());
            }
        }

        // 从存储加载
        let loaded = self
            .store
            .load_session(session_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Session not found: {session_id}"))?;

        let managed = Arc::new(ManagedSession::new(loaded));
        let mut map = self.sessions.write().await;
        let entry = map.entry(session_id.to_string()).or_insert_with(|| managed.clone());
        Ok(entry.clone())
    }

    /// 创建并持久化新 Session
    pub async fn create_session(
        &self,
        model: String,
        variant: Option<String>,
        title: Option<String>,
    ) -> Result<Arc<ManagedSession>, String> {
        let session = Session::with_model(&self.workspace, title, model, variant);
        self.store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;

        let managed = Arc::new(ManagedSession::new(session));
        let mut map = self.sessions.write().await;
        map.insert(managed.id.clone(), managed.clone());
        Ok(managed)
    }

    /// 为指定 Session 启动一轮后台执行任务
    /// 无论当前 UI 是否正在观察此 Session，后台任务均独立运行
    pub async fn submit_prompt(
        &self,
        managed: Arc<ManagedSession>,
        prompt: &str,
        web_search: bool,
    ) -> Result<(), String> {
        if managed.is_running() {
            return Err("Session is currently running another prompt".to_string());
        }

        // 1. 追加 User Turn
        let user_msg = Message::user(prompt);
        let parent_turn_id = {
            let s = managed.session.read().await;
            s.last_turn_id()
        };
        let user_turn = Turn::user_turn(user_msg, parent_turn_id);

        self.store
            .append_turn(&managed.id, &user_turn)
            .await
            .map_err(|e| e.to_string())?;

        {
            let mut s = managed.session.write().await;
            // 若 session 尚无 title，用前 40 字符填充
            if s.title.is_none() {
                s.title = Some(prompt.chars().take(40).collect());
            }
            s.append_turn(user_turn);
        }

        // 2. 解析 Provider 与参数
        let (current_model, current_variant, session_workspace) = {
            let s = managed.session.read().await;
            (s.current_model.clone(), s.current_variant.clone(), s.workspace_root.clone())
        };

        let (provider, effective_model, reasoning_effort) =
            resolve_provider(&self.app_config, &current_model, current_variant.as_deref())?;

        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(BashTool::new(&session_workspace)));
        tools.register(Arc::new(FileViewTool::new(&session_workspace)));
        tools.register(Arc::new(FileEditTool::new(&session_workspace)));

        let agent_config = AgentConfig {
            max_iterations: 15,
            completion_options: CompletionOptions {
                model: effective_model,
                enable_web_search: web_search,
                reasoning_effort,
                ..Default::default()
            },
        };

        let engine = AgentEngine::new(provider, self.store.clone(), tools, agent_config);

        // 3. 标记运行中并启动后台任务
        managed.is_running.store(true, Ordering::SeqCst);
        *managed.status_text.write().await = "Thinking...".to_string();

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
        let event_tx = managed.event_tx.clone();
        let status_text = managed.status_text.clone();
        let is_running = managed.is_running.clone();
        let session_lock = managed.session.clone();

        // 启动事件转发小任务
        let fwd_status = status_text.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match &event {
                    AgentEvent::Thought(_) => {
                        *fwd_status.write().await = "Thinking...".to_string();
                    }
                    AgentEvent::ToolExecuting { name, .. } => {
                        *fwd_status.write().await = format!("Executing tool: {name}");
                    }
                    AgentEvent::ToolFinished { name, is_error, .. } => {
                        let sym = if *is_error { "failed" } else { "done" };
                        *fwd_status.write().await = format!("Tool {name} {sym}");
                    }
                    AgentEvent::Text(_) => {
                        *fwd_status.write().await = "Responding...".to_string();
                    }
                    AgentEvent::Finished => {
                        *fwd_status.write().await = String::new();
                    }
                }
                let _ = event_tx.send(event);
            }
        });

        // 启动主 Agent 驱动任务
        let task = tokio::spawn(async move {
            let res = {
                let mut session_guard = session_lock.write().await;
                engine.run_turn(&mut session_guard, Some(tx)).await
            };

            if let Err(e) = res {
                error!("Agent run_turn failed: {e}");
            }

            is_running.store(false, Ordering::SeqCst);
            *status_text.write().await = String::new();
        });

        let mut abort_handle = managed.task_abort_handle.lock().await;
        *abort_handle = Some(task.abort_handle());

        Ok(())
    }
}

pub type ResolvedProviderInfo = (Arc<dyn Provider>, String, Option<String>);

/// 解析 Provider 实例、模型生效名称及 Variant
pub fn resolve_provider(
    app_config: &Option<AppConfig>,
    current_model: &str,
    current_variant: Option<&str>,
) -> Result<ResolvedProviderInfo, String> {
    if let Some(cfg) = app_config {
        match cfg.resolve_model(current_model, current_variant) {
            Ok(resolved) => {
                let ResolvedModel {
                    model_id,
                    provider,
                    selected_variant,
                    ..
                } = resolved;
                Ok((provider, model_id, selected_variant))
            }
            Err(err) => Err(format!("Error resolving model '{current_model}': {err}")),
        }
    } else {
        // 无配置文件时的环境变数备用路由
        let (provider_inst, model_id): (Arc<dyn Provider>, String) = match current_model.to_lowercase().as_str() {
            m if m.contains("anthropic") || m.contains("claude") => {
                let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                (Arc::new(AnthropicProvider::new(key)), current_model.to_string())
            }
            m if m.contains("gemini") => {
                let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
                (Arc::new(GeminiProvider::new(key)), current_model.to_string())
            }
            _ => {
                let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                (Arc::new(ResponseProvider::new(key)), current_model.to_string())
            }
        };
        Ok((provider_inst, model_id, current_variant.map(String::from)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::jsonl::JsonlSessionStore;

    #[tokio::test]
    async fn test_session_manager_singleton_guarantee() {
        let temp_dir = std::env::temp_dir().join(format!("vva_test_mgr_{}", uuid::Uuid::new_v4().simple()));
        let store = Arc::new(JsonlSessionStore::new(&temp_dir).expect("init store"));
        let mgr = SessionManager::new(store.clone(), None, "/tmp/project");

        let s1 = mgr.create_session("model_a".into(), Some("high".into()), Some("Title 1".into())).await.expect("create s1");
        let sid = s1.id.clone();

        // 再次加载同一个 session，必须返回相同单例指针
        let s2 = mgr.get_or_load(&sid).await.expect("get s1");
        assert!(Arc::ptr_eq(&s1, &s2));

        // 清理临时目录
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
