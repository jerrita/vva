use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::error;

use crate::config::{AppConfig, ResolvedModel};
use crate::harness::{AgentConfig, AgentEngine, AgentEvent};
use crate::model::{ContentPart, Message, Session, Turn};
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::gemini::GeminiProvider;
use crate::provider::response::ResponseProvider;
use crate::provider::{CompletionOptions, Provider};
use crate::storage::SessionStore;
use crate::tools::ToolRegistry;
#[cfg(not(windows))]
use crate::tools::bash::BashTool;
use crate::tools::file::{FileEditTool, FileViewTool};
#[cfg(windows)]
use crate::tools::powershell::PowerShellTool;

/// 单个活跃会话的运行时单例句柄
pub struct ManagedSession {
    pub id: String,
    pub session: Arc<RwLock<Session>>,
    pub is_running: Arc<AtomicBool>,
    pub is_persisted: Arc<AtomicBool>,
    is_deleted: Arc<AtomicBool>,
    pub status_text: Arc<RwLock<String>>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    pub task_abort_handle: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
}

impl ManagedSession {
    pub fn new(session: Session) -> Self {
        Self::new_with_persisted(session, true)
    }

    pub fn new_unpersisted(session: Session) -> Self {
        Self::new_with_persisted(session, false)
    }

    fn new_with_persisted(session: Session, persisted: bool) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let id = session.id.clone();
        Self {
            id,
            session: Arc::new(RwLock::new(session)),
            is_running: Arc::new(AtomicBool::new(false)),
            is_persisted: Arc::new(AtomicBool::new(persisted)),
            is_deleted: Arc::new(AtomicBool::new(false)),
            status_text: Arc::new(RwLock::new(String::new())),
            event_tx,
            task_abort_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_persisted(&self) -> bool {
        self.is_persisted.load(Ordering::SeqCst)
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
        let entry = map
            .entry(session_id.to_string())
            .or_insert_with(|| managed.clone());
        Ok(entry.clone())
    }

    /// 创建内存中待持久化的新 Session（发送首条消息时真正写入存储）
    pub async fn create_session(
        &self,
        model: String,
        variant: Option<String>,
        title: Option<String>,
        web_search: bool,
    ) -> Result<Arc<ManagedSession>, String> {
        let session = Session::with_model(&self.workspace, title, model, variant, web_search);
        let managed = Arc::new(ManagedSession::new_unpersisted(session));
        let mut map = self.sessions.write().await;
        map.insert(managed.id.clone(), managed.clone());
        Ok(managed)
    }

    /// 终止并删除一个 Session，防止后台任务在删除后重新写入数据。
    pub async fn delete_session(&self, managed: &Arc<ManagedSession>) -> Result<(), String> {
        managed.is_deleted.store(true, Ordering::SeqCst);
        managed.abort_current().await;

        // 等待执行与标题生成任务释放 Session 写锁，避免删除后继续落盘。
        let session_guard = managed.session.write().await;
        if managed.is_persisted() {
            self.store
                .delete_session(&managed.id)
                .await
                .map_err(|e| e.to_string())?;
        }
        drop(session_guard);

        self.sessions.write().await.remove(&managed.id);
        Ok(())
    }

    /// 为指定 Session 启动一轮后台执行任务
    /// 后台任务在独立任务中运行，与当前 UI 聚焦状态解耦
    pub async fn submit_prompt(
        &self,
        managed: Arc<ManagedSession>,
        prompt: &str,
    ) -> Result<(), String> {
        if managed.is_deleted.load(Ordering::SeqCst) {
            return Err("Session has been deleted".to_string());
        }
        if managed.is_running() {
            return Err("Session is currently running another prompt".to_string());
        }

        // 1. 如果尚未持久化（新会话第一条消息），固定生成系统提示词并写入 Header
        let mut should_generate_title = false;
        let parent_turn_id = if !managed.is_persisted() {
            let mut s = managed.session.write().await;

            let sys_prompt = crate::system::build_system_prompt(&s.workspace_root);
            let sys_turn = Turn::new(
                crate::model::Role::System,
                vec![Message::system(sys_prompt)],
                None,
            );
            let sys_turn_id = sys_turn.id.clone();
            s.append_turn(sys_turn);

            if s.title.is_none() {
                s.title = Some(prompt.chars().take(40).collect());
                should_generate_title = true;
            }

            self.store
                .save_header(&s)
                .await
                .map_err(|e| e.to_string())?;

            managed.is_persisted.store(true, Ordering::SeqCst);
            Some(sys_turn_id)
        } else {
            let s = managed.session.read().await;
            s.last_turn_id()
        };

        // 2. 追加 User Turn
        let user_msg = Message::user(prompt);
        let user_turn = Turn::user_turn(user_msg, parent_turn_id);

        self.store
            .append_turn(&managed.id, &user_turn)
            .await
            .map_err(|e| e.to_string())?;

        {
            let mut s = managed.session.write().await;
            if s.title.is_none() {
                s.title = Some(prompt.chars().take(40).collect());
                should_generate_title = true;
            }
            s.append_turn(user_turn);
        }

        if should_generate_title {
            let cfg_clone = self.app_config.clone();
            let store_clone = self.store.clone();
            let session_clone = managed.session.clone();
            let is_deleted = managed.is_deleted.clone();
            let prompt_str = prompt.to_string();
            tokio::spawn(async move {
                generate_title_with_small_model(
                    &cfg_clone,
                    store_clone,
                    session_clone,
                    is_deleted,
                    prompt_str,
                )
                .await;
            });
        }

        // 2. 解析 Provider 与参数
        let (current_model, current_variant, session_workspace, session_web_search) = {
            let s = managed.session.read().await;
            (
                s.current_model.clone(),
                s.current_variant.clone(),
                s.workspace_root.clone(),
                s.web_search,
            )
        };

        let (provider, effective_model, reasoning_effort) =
            resolve_provider(&self.app_config, &current_model, current_variant.as_deref())?;

        let mut tools = ToolRegistry::new();
        #[cfg(windows)]
        tools.register(Arc::new(PowerShellTool::new(&session_workspace)));
        #[cfg(not(windows))]
        tools.register(Arc::new(BashTool::new(&session_workspace)));
        tools.register(Arc::new(FileViewTool::new(&session_workspace)));
        tools.register(Arc::new(FileEditTool::new(&session_workspace)));

        let agent_config = AgentConfig {
            max_iterations: 15,
            completion_options: CompletionOptions {
                model: effective_model,
                enable_web_search: session_web_search,
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
                    AgentEvent::Citation { .. } => {}
                    AgentEvent::Finished => {
                        *fwd_status.write().await = String::new();
                    }
                    AgentEvent::Error(_) => {
                        *fwd_status.write().await = String::new();
                    }
                }
                let _ = event_tx.send(event);
            }
        });

        // 启动主 Agent 驱动任务
        let task_event_tx = managed.event_tx.clone();
        let task = tokio::spawn(async move {
            let res = {
                let mut session_guard = session_lock.write().await;
                engine.run_turn(&mut session_guard, Some(tx)).await
            };

            if let Err(e) = res {
                error!("Agent run_turn failed: {e}");
                let _ = task_event_tx.send(AgentEvent::Error(format!("Agent error: {e}")));
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
            Err(err) => {
                // 如果历史会话中记录的模型在当前配置中不存在，回退到默认模型
                if current_model != cfg.settings.default
                    && let Ok(fallback) = cfg.resolve_model(&cfg.settings.default, None)
                {
                    tracing::warn!(
                        "Model '{current_model}' not found in current config; falling back to default '{}'",
                        cfg.settings.default
                    );
                    return Ok((
                        fallback.provider,
                        fallback.model_id,
                        fallback.selected_variant,
                    ));
                }
                Err(format!("Error resolving model '{current_model}': {err}"))
            }
        }
    } else {
        // 无配置文件时的环境变数备用路由
        let (provider_inst, model_id): (Arc<dyn Provider>, String) =
            match current_model.to_lowercase().as_str() {
                m if m.contains("anthropic") || m.contains("claude") => {
                    let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                    (
                        Arc::new(AnthropicProvider::new(key)),
                        current_model.to_string(),
                    )
                }
                m if m.contains("gemini") => {
                    let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
                    (
                        Arc::new(GeminiProvider::new(key)),
                        current_model.to_string(),
                    )
                }
                _ => {
                    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                    (
                        Arc::new(ResponseProvider::new(key)),
                        current_model.to_string(),
                    )
                }
            };
        Ok((provider_inst, model_id, current_variant.map(String::from)))
    }
}

/// 使用 small_model 为 Session 生成精简标题（非流式调用）
pub async fn generate_title_with_small_model(
    app_config: &Option<AppConfig>,
    store: Arc<dyn SessionStore>,
    session_lock: Arc<RwLock<Session>>,
    is_deleted: Arc<AtomicBool>,
    user_prompt: String,
) {
    let small_model_spec = app_config
        .as_ref()
        .and_then(|c| c.settings.small_model.as_deref());

    let (provider, effective_model) = match small_model_spec {
        Some(spec) => match resolve_provider(app_config, spec, None) {
            Ok((p, m, _)) => (p, m),
            Err(e) => {
                tracing::warn!("Failed to resolve small_model '{spec}' for title generation: {e}");
                return;
            }
        },
        None => {
            let current_model = {
                let s = session_lock.read().await;
                s.current_model.clone()
            };
            if current_model.is_empty() {
                return;
            }
            match resolve_provider(app_config, &current_model, None) {
                Ok((p, m, _)) => (p, m),
                Err(e) => {
                    tracing::warn!(
                        "Failed to resolve model '{current_model}' for title generation: {e}"
                    );
                    return;
                }
            }
        }
    };

    let title_system = "You are a concise title generator. Generate a very brief title (3-6 words, plain text, no markdown, no quotes, no trailing punctuation) summarizing the user's prompt. Return ONLY the title text.";
    let mut title_session = Session::new(".", None);
    title_session.append_turn(Turn::user_turn(
        Message::user(format!("{title_system}\n\nUser prompt:\n{user_prompt}")),
        None,
    ));

    let options = CompletionOptions {
        model: effective_model,
        temperature: Some(0.3),
        max_tokens: Some(30),
        enable_web_search: false,
        reasoning_effort: None,
    };

    // 非流式调用 (标题生成除外)
    match provider.complete(&title_session, &[], &options).await {
        Ok(resp) => {
            let mut title_candidate = String::new();
            for msg in resp.messages {
                for part in msg.parts {
                    if let ContentPart::Text { text, .. } = part {
                        title_candidate.push_str(&text);
                    }
                }
            }

            let cleaned = title_candidate
                .trim()
                .trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '#' || c == '*')
                .lines()
                .next()
                .unwrap_or("")
                .trim();

            if !cleaned.is_empty() {
                if is_deleted.load(Ordering::SeqCst) {
                    return;
                }
                let mut s = session_lock.write().await;
                if is_deleted.load(Ordering::SeqCst) {
                    return;
                }
                s.title = Some(cleaned.to_string());
                if let Err(e) = store.save_header(&s).await {
                    tracing::warn!("Failed to persist generated session title: {e}");
                }
            }
        }
        Err(e) => {
            tracing::warn!("small_model title generation failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::jsonl::JsonlSessionStore;

    #[tokio::test]
    async fn test_session_manager_singleton_guarantee() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_test_mgr_{}", uuid::Uuid::new_v4().simple()));
        let store = Arc::new(JsonlSessionStore::new(&temp_dir).expect("init store"));
        let mgr = SessionManager::new(store.clone(), None, "/tmp/project");

        let s1 = mgr
            .create_session(
                "model_a".into(),
                Some("high".into()),
                Some("Title 1".into()),
                false,
            )
            .await
            .expect("create s1");
        let sid = s1.id.clone();

        // 再次加载同一个 session，返回相同单例指针
        let s2 = mgr.get_or_load(&sid).await.expect("get s1");
        assert!(Arc::ptr_eq(&s1, &s2));

        // 清理临时目录
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_small_model_title_generation_flow() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_test_title_{}", uuid::Uuid::new_v4().simple()));
        let store = Arc::new(JsonlSessionStore::new(&temp_dir).expect("init store"));

        let session = Session::with_model(&temp_dir, None, "test_model", None, false);
        store.save_header(&session).await.expect("save header");

        let session_lock = Arc::new(RwLock::new(session));
        assert_eq!(session_lock.read().await.title, None);

        generate_title_with_small_model(
            &None,
            store.clone(),
            session_lock.clone(),
            Arc::new(AtomicBool::new(false)),
            "How to write a fast web server in Rust?".into(),
        )
        .await;

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_lazy_session_creation_persists_only_on_first_prompt() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_test_lazy_{}", uuid::Uuid::new_v4().simple()));
        let store = Arc::new(JsonlSessionStore::new(&temp_dir).expect("init store"));
        let mgr = SessionManager::new(store.clone(), None, &temp_dir);

        // 1. 创建 Session 时不落盘
        let s = mgr
            .create_session("response/gpt-4.5".into(), None, None, false)
            .await
            .expect("create s");
        assert!(!s.is_persisted());

        let session_file = temp_dir.join(format!("{}.jsonl", s.id));
        assert!(
            !session_file.exists(),
            "Session file should not exist before first message"
        );

        let sessions = store.list_sessions().await.expect("list sessions");
        assert_eq!(
            sessions.len(),
            0,
            "Store should have 0 sessions before first message"
        );

        // 2. 提交第一条消息后真正落盘，并生成 Turn 0 (System) 与 Turn 1 (User)
        mgr.submit_prompt(s.clone(), "First prompt")
            .await
            .expect("submit prompt");
        assert!(s.is_persisted());
        assert!(
            session_file.exists(),
            "Session file should exist after first prompt"
        );

        let loaded = store
            .load_session(&s.id)
            .await
            .expect("load")
            .expect("must exist");
        assert_eq!(loaded.turns.len(), 2);
        assert_eq!(loaded.turns[0].role, crate::model::Role::System);
        assert_eq!(loaded.turns[1].role, crate::model::Role::User);
        assert!(
            loaded.turns[0].messages[0]
                .text_content()
                .contains("## Environment Context")
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
