use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot, watch};

use super::agent::{AgentEngine, capability_notices, generate_title};
use crate::config::AppConfig;
use crate::event::AgentEvent;
use crate::model::{Annotation, ContentPart, Message, Role, Session, Turn};
use crate::storage::SessionStore;

type Reply = oneshot::Sender<Result<(), String>>;

/// 流式增量的视图发布间隔（终端重绘为 100ms，此处取其三分之一）
const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Debug, Clone, Default)]
pub enum RunState {
    #[default]
    Idle,
    Thinking,
    Responding,
    Tool(String),
}

impl RunState {
    pub fn status(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Thinking => "Thinking...".into(),
            Self::Responding => "Responding...".into(),
            Self::Tool(name) => format!("Executing tool: {name}"),
        }
    }
}

/// 会话任务发布的只读视图；草稿与已完成消息使用相同的内容类型。
/// `revision` 每次发布递增，供订阅方判定缓存是否失效。
#[derive(Clone)]
pub struct SessionView {
    pub revision: u64,
    pub session: Arc<Session>,
    pub draft: Vec<ContentPart>,
    pub notices: Vec<String>,
    pub state: RunState,
    pub persisted: bool,
}

enum Command {
    Submit {
        prompt: String,
        events: Option<mpsc::Sender<AgentEvent>>,
        reply: Reply,
    },
    Configure {
        model: Option<String>,
        variant: Option<Option<String>>,
        search: Option<bool>,
        reply: Reply,
    },
    Stop {
        delete: bool,
        reply: Reply,
    },
    Title(String),
}

pub struct ManagedSession {
    pub id: String,
    commands: mpsc::Sender<Command>,
    view: watch::Receiver<Arc<SessionView>>,
}

impl ManagedSession {
    pub fn snapshot(&self) -> Arc<SessionView> {
        self.view.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<SessionView>> {
        self.view.clone()
    }

    pub fn is_running(&self) -> bool {
        !matches!(self.view.borrow().state, RunState::Idle)
    }

    pub fn is_persisted(&self) -> bool {
        self.view.borrow().persisted
    }

    async fn request(&self, command: impl FnOnce(Reply) -> Command) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(command(tx))
            .await
            .map_err(|_| "Session is closed".to_string())?;
        rx.await.map_err(|_| "Session task stopped".to_string())?
    }

    pub async fn submit(
        &self,
        prompt: &str,
        events: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<(), String> {
        self.request(|reply| Command::Submit {
            prompt: prompt.into(),
            events,
            reply,
        })
        .await
    }

    pub async fn configure(
        &self,
        model: Option<String>,
        variant: Option<Option<String>>,
        search: Option<bool>,
    ) -> Result<(), String> {
        self.request(|reply| Command::Configure {
            model,
            variant,
            search,
            reply,
        })
        .await
    }

    pub async fn abort_current(&self) -> Result<(), String> {
        self.request(|reply| Command::Stop {
            delete: false,
            reply,
        })
        .await
    }
}

/// 只管理会话句柄；Session 的修改与存储由对应任务顺序执行。
pub struct SessionManager {
    store: Arc<dyn SessionStore>,
    config: Arc<AppConfig>,
    workspace: PathBuf,
    sessions: Mutex<HashMap<String, Arc<ManagedSession>>>,
}

impl SessionManager {
    pub fn new(
        store: Arc<dyn SessionStore>,
        config: AppConfig,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            config: Arc::new(config),
            workspace: workspace.into(),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> &Arc<dyn SessionStore> {
        &self.store
    }
    pub fn app_config(&self) -> &AppConfig {
        &self.config
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
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
        };
        let (view_tx, view_rx) = watch::channel(Arc::new(view.clone()));
        let (commands, rx) = mpsc::channel(16);
        let task = SessionTask {
            view,
            view_tx,
            store: self.store.clone(),
            config: self.config.clone(),
            commands: commands.downgrade(),
            dirty: false,
            cli: None,
            stop: None,
            notified: HashSet::new(),
            title_task: None,
        };
        tokio::spawn(task.run(rx));
        Arc::new(ManagedSession {
            id,
            commands,
            view: view_rx,
        })
    }

    pub async fn get_or_load(&self, id: &str) -> Result<Arc<ManagedSession>, String> {
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
        &self,
        model: String,
        variant: Option<String>,
        title: Option<String>,
        search: bool,
    ) -> Result<Arc<ManagedSession>, String> {
        let resolved = self
            .config
            .resolve_model(&model, variant.as_deref())
            .map_err(|e| e.to_string())?;
        let session =
            Session::with_model(&self.workspace, title, resolved.full_spec, variant, search);
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
                self.create_session(
                    model.unwrap_or_else(|| self.config.settings.default.clone()),
                    variant,
                    None,
                    search.unwrap_or(true),
                )
                .await
            }
        }
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

    pub async fn submit_prompt(
        &self,
        handle: Arc<ManagedSession>,
        prompt: &str,
    ) -> Result<(), String> {
        handle.submit(prompt, None).await
    }
}

struct SessionTask {
    view: SessionView,
    view_tx: watch::Sender<Arc<SessionView>>,
    store: Arc<dyn SessionStore>,
    config: Arc<AppConfig>,
    commands: mpsc::WeakSender<Command>,
    /// 流式增量只置脏，由 drive 的节流 tick 或结构性变化触发发布
    dirty: bool,
    cli: Option<mpsc::Sender<AgentEvent>>,
    stop: Option<(bool, Reply)>,
    notified: HashSet<String>,
    title_task: Option<tokio::task::JoinHandle<()>>,
}

impl SessionTask {
    fn publish(&mut self) {
        self.dirty = false;
        self.view.revision += 1;
        self.view_tx.send_replace(Arc::new(self.view.clone()));
    }

    /// 流式增量期间累积改动，由节流点统一发布，避免每个 token 复制整份草稿
    fn publish_throttled(&mut self) {
        if self.dirty {
            self.publish();
        }
    }

    async fn event(&mut self, event: AgentEvent) {
        // 文本与思考增量按节流发布，其余事件改变结构或状态，立即发布
        let is_delta = matches!(event, AgentEvent::Thought(_) | AgentEvent::Text(_));
        match &event {
            AgentEvent::Thought(text) => {
                self.view.state = RunState::Thinking;
                if let Some(ContentPart::Reasoning { content, .. }) = self.view.draft.last_mut() {
                    content.push_str(text);
                } else {
                    self.view.draft.push(ContentPart::reasoning(text, None));
                }
            }
            AgentEvent::Text(text) => {
                self.view.state = RunState::Responding;
                if let Some(ContentPart::Text { text: content, .. }) = self.view.draft.last_mut() {
                    content.push_str(text);
                } else {
                    self.view.draft.push(ContentPart::text(text));
                }
            }
            AgentEvent::Citation { title, url } => {
                if !matches!(self.view.draft.last(), Some(ContentPart::Text { .. })) {
                    self.view.draft.push(ContentPart::text(""));
                }
                if let Some(ContentPart::Text { annotations, .. }) = self.view.draft.last_mut()
                    && !annotations
                        .iter()
                        .any(|a| matches!(a, Annotation::UrlCitation { url: u, .. } if u == url))
                {
                    annotations.push(Annotation::UrlCitation {
                        title: title.clone(),
                        url: url.clone(),
                        start_index: 0,
                        end_index: 0,
                    });
                }
            }
            AgentEvent::ToolExecuting { name, .. } => {
                self.view.state = RunState::Tool(name.clone())
            }
            AgentEvent::ToolFinished { .. } => {}
            AgentEvent::Notice(text) | AgentEvent::Error(text) => {
                self.view.notices.push(text.clone())
            }
            AgentEvent::Aborted => {
                // 未落盘的草稿随取消一并丢弃，界面与存储保持一致
                self.view.draft.clear();
                self.view.notices.push("Turn aborted.".into());
            }
            AgentEvent::Finished => {}
        }
        if is_delta {
            self.dirty = true;
        } else {
            self.publish();
        }
        if let Some(tx) = &self.cli {
            let _ = tx.send(event).await;
        }
    }

    async fn append(&mut self, turn: Turn) -> Result<(), String> {
        self.store
            .append_turn(&self.view.session.id, &turn)
            .await
            .map_err(|e| e.to_string())?;
        Arc::make_mut(&mut self.view.session).append_turn(turn);
        self.publish();
        Ok(())
    }

    async fn configure(
        &mut self,
        model: Option<String>,
        variant: Option<Option<String>>,
        search: Option<bool>,
    ) -> Result<(), String> {
        if model.is_none() && variant.is_none() && search.is_none() {
            return Ok(());
        }
        let mut session = (*self.view.session).clone();
        if model.is_some() || variant.is_some() {
            let target = model.as_deref().unwrap_or(&session.current_model);
            let selected = variant.unwrap_or_else(|| session.current_variant.clone());
            let resolved = self
                .config
                .resolve_model(target, selected.as_deref())
                .map_err(|e| e.to_string())?;
            session.set_model(resolved.full_spec, selected);
        }
        if let Some(search) = search {
            session.set_web_search(search);
        }
        if self.view.persisted {
            self.store
                .save_header(&session)
                .await
                .map_err(|e| e.to_string())?;
        }
        self.view.session = Arc::new(session);
        self.publish();
        Ok(())
    }

    async fn title(&mut self, title: String) -> Result<(), String> {
        if title.is_empty() {
            return Ok(());
        }
        let mut session = (*self.view.session).clone();
        session.title = Some(title);
        self.store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;
        self.view.session = Arc::new(session);
        self.publish();
        Ok(())
    }

    async fn start(&mut self, prompt: &str) -> Result<AgentEngine, String> {
        let engine = AgentEngine::new(&self.config, &self.view.session)?;
        let generate = self.view.session.title.is_none();
        if !self.view.persisted {
            let mut session = (*self.view.session).clone();
            let workspace = session.workspace_root.clone();
            let system =
                tokio::task::spawn_blocking(move || crate::system::build_system_prompt(&workspace))
                    .await
                    .map_err(|e| e.to_string())?;
            session.append_turn(Turn::new(Role::System, vec![Message::text(system)]));
            if generate {
                session.title = Some(crate::model::provisional_title(prompt));
            }
            self.store
                .save_header(&session)
                .await
                .map_err(|e| e.to_string())?;
            self.view.session = Arc::new(session);
            self.view.persisted = true;
        }
        self.view.draft.clear();
        self.append(Turn::user_turn(Message::text(prompt))).await?;
        self.view.state = RunState::Thinking;
        self.publish();
        if generate {
            let config = self.config.clone();
            let model = self.view.session.current_model.clone();
            let prompt = prompt.to_string();
            let commands = self.commands.clone();
            self.title_task = Some(tokio::spawn(async move {
                match generate_title(&config, &model, &prompt).await {
                    Ok(title) => {
                        if let Some(tx) = commands.upgrade() {
                            let _ = tx.send(Command::Title(title)).await;
                        }
                    }
                    Err(error) => tracing::warn!("Title generation failed: {error}"),
                }
            }));
        }
        Ok(engine)
    }

    /// 网络与工具等待期间处理设置和取消；磁盘提交在本任务中顺序完成。
    /// 流式增量以 PUBLISH_INTERVAL 为节奏发布，慢于终端重绘频率即无可见差异。
    async fn drive<T>(
        &mut self,
        future: impl Future<Output = T>,
        commands: &mut mpsc::Receiver<Command>,
        events: &mut mpsc::Receiver<AgentEvent>,
    ) -> Option<T> {
        tokio::pin!(future);
        let mut publish_tick = tokio::time::interval(PUBLISH_INTERVAL);
        loop {
            tokio::select! {
                result = &mut future => {
                    while let Ok(event) = events.try_recv() { self.event(event).await; }
                    self.publish_throttled();
                    return Some(result);
                }
                Some(event) = events.recv() => self.event(event).await,
                _ = publish_tick.tick() => self.publish_throttled(),
                command = commands.recv() => match command {
                    Some(Command::Configure { model, variant, search, reply }) => { let _ = reply.send(self.configure(model, variant, search).await); }
                    Some(Command::Title(title)) => if let Err(error) = self.title(title).await { self.event(AgentEvent::Notice(error)).await; },
                    Some(Command::Submit { reply, .. }) => { let _ = reply.send(Err("Session is currently running another prompt".into())); }
                    Some(Command::Stop { delete, reply }) => { self.stop = Some((delete, reply)); self.publish_throttled(); return None; }
                    None => { self.publish_throttled(); return None; }
                }
            }
        }
    }

    async fn execute(
        &mut self,
        engine: AgentEngine,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<bool, String> {
        for notice in capability_notices(engine.provider.as_ref(), &engine.options) {
            if self.notified.insert(notice.clone()) {
                self.event(AgentEvent::Notice(notice)).await;
            }
        }
        let tools = engine.tools.definitions();
        for _ in 0..15 {
            self.view.state = RunState::Thinking;
            self.publish();
            let context = self.view.session.clone();
            let (tx, mut rx) = mpsc::channel(64);
            let request =
                engine
                    .provider
                    .complete_stream(&context, &tools, &engine.options, Some(tx));
            let Some(response) = self.drive(request, commands, &mut rx).await else {
                return Ok(false);
            };
            let response = response.map_err(|e| e.to_string())?;
            let calls: Vec<_> = response
                .messages
                .iter()
                .flat_map(|m| &m.parts)
                .filter_map(|p| {
                    if let ContentPart::ToolCall(call) = p {
                        Some(call.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let mut turn = Turn::assistant_turn(response.messages);
            turn.usage = response.usage;
            turn.meta = response.meta;
            self.view.draft.clear();
            self.append(turn).await?;
            if calls.is_empty() {
                return Ok(true);
            }
            for call in calls {
                self.event(AgentEvent::ToolExecuting {
                    name: call.name.clone(),
                    args: call.arguments.clone(),
                })
                .await;
                let Some(result) = self
                    .drive(engine.tools.execute_call(&call), commands, &mut rx)
                    .await
                else {
                    return Ok(false);
                };
                self.event(AgentEvent::ToolFinished {
                    name: call.name,
                    result: result.content.clone(),
                    is_error: result.is_error,
                })
                .await;
                self.append(Turn::new(Role::Tool, vec![Message::tool_result(result)]))
                    .await?;
            }
        }
        self.event(AgentEvent::Notice(
            "Agent loop reached the iteration limit.".into(),
        ))
        .await;
        Ok(true)
    }

    async fn stop(&mut self, delete: bool, reply: Reply) -> bool {
        let result = if delete && self.view.persisted {
            self.store
                .delete_session(&self.view.session.id)
                .await
                .map_err(|e| e.to_string())
        } else {
            Ok(())
        };
        let close = delete && result.is_ok();
        let _ = reply.send(result);
        close
    }

    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        while let Some(command) = commands.recv().await {
            match command {
                Command::Submit {
                    prompt,
                    events,
                    reply,
                } => {
                    let engine = match self.start(&prompt).await {
                        Ok(engine) => {
                            let _ = reply.send(Ok(()));
                            engine
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                    self.cli = events;
                    let result = self.execute(engine, &mut commands).await;
                    self.view.state = RunState::Idle;
                    match result {
                        Ok(true) => self.event(AgentEvent::Finished).await,
                        Ok(false) => self.event(AgentEvent::Aborted).await,
                        Err(error) => self.event(AgentEvent::Error(error)).await,
                    }
                    self.cli = None;
                    if let Some((delete, reply)) = self.stop.take()
                        && self.stop(delete, reply).await
                    {
                        break;
                    }
                }
                Command::Configure {
                    model,
                    variant,
                    search,
                    reply,
                } => {
                    let _ = reply.send(self.configure(model, variant, search).await);
                }
                Command::Title(title) => {
                    if let Err(error) = self.title(title).await {
                        self.event(AgentEvent::Notice(error)).await;
                    }
                }
                Command::Stop { delete, reply } => {
                    if self.stop(delete, reply).await {
                        break;
                    }
                }
            }
        }
        if let Some(task) = self.title_task {
            task.abort();
        }
    }
}
