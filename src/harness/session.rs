use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

use super::agent::{AgentEngine, capability_notices, generate_title};
use crate::config::{AppConfig, AppState};
use crate::event::AgentEvent;
use crate::model::{ContentPart, MediaSource, Message, Role, Session, Turn};
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
///
/// 草稿部件以 `Arc` 共享：发布只复制指针序列，修改单个部件仅克隆该部件。
/// 部件身份（`Arc::ptr_eq`）即内容未变判定，订阅方（Web 增量、TUI 渲染缓存）
/// 以身份对齐前缀，避免逐内容比较。文本增量只做追加，保证同身份部件的内容
/// 前缀稳定。
#[derive(Clone)]
pub struct SessionView {
    pub revision: u64,
    pub session: Arc<Session>,
    pub draft: Vec<Arc<ContentPart>>,
    pub notices: Vec<String>,
    pub state: RunState,
    pub persisted: bool,
}

enum Command {
    Submit {
        prompt: String,
        /// 用户随提示词提交的图片附件（Base64）
        images: Vec<ContentPart>,
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
    Revert {
        turn_index: usize,
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
        images: Vec<ContentPart>,
        events: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<(), String> {
        self.request(|reply| Command::Submit {
            prompt: prompt.into(),
            images,
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

    pub async fn revert(&self, turn_index: usize) -> Result<(), String> {
        self.request(|reply| Command::Revert { turn_index, reply })
            .await
    }
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
            config: self.app_config(),
            state_path: self.state_path.clone(),
            commands: commands.downgrade(),
            dirty: false,
            cli: None,
            stop: None,
            notified: HashSet::new(),
            title_task: None,
            text_parts: HashMap::new(),
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
        for _ in 0..8 {
            if self
                .store
                .load_session(&session.id)
                .await
                .map_err(|e| e.to_string())?
                .is_none()
            {
                break;
            }
            session.id = crate::model::generate_session_id();
        }
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
        if let Some(model) = model {
            return (model, variant);
        }
        let state = AppState::load(&self.state_path);
        if !state.last_model.is_empty() {
            let inherited = variant.clone().or(state.last_variant.clone());
            let config = self.app_config();
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
        (self.app_config().settings.default.clone(), variant)
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
        for _ in 0..8 {
            if self
                .store
                .load_session(&session.id)
                .await
                .map_err(|e| e.to_string())?
                .is_none()
            {
                break;
            }
            session.id = crate::model::generate_session_id();
        }
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

struct SessionTask {
    view: SessionView,
    view_tx: watch::Sender<Arc<SessionView>>,
    store: Arc<dyn SessionStore>,
    config: Arc<AppConfig>,
    state_path: PathBuf,
    commands: mpsc::WeakSender<Command>,
    /// 流式增量只置脏，由 drive 的节流 tick 或结构性变化触发发布
    dirty: bool,
    cli: Option<mpsc::Sender<AgentEvent>>,
    stop: Option<(bool, Reply)>,
    notified: HashSet<String>,
    title_task: Option<tokio::task::JoinHandle<()>>,
    /// 响应内文本标识到草稿部件的映射，供增量与引用共同定位。
    text_parts: HashMap<String, usize>,
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

    fn text_part_index(&mut self, text_id: &str, relocate: bool) -> usize {
        let index = if let Some(&index) = self.text_parts.get(text_id) {
            index
        } else {
            let index = self.view.draft.len();
            self.view.draft.push(Arc::new(ContentPart::text("")));
            self.text_parts.insert(text_id.to_string(), index);
            index
        };
        if relocate {
            self.relocate_text_part(index)
        } else {
            index
        }
    }

    fn relocate_text_part(&mut self, index: usize) -> usize {
        if index + 1 == self.view.draft.len()
            || self.view.draft[index + 1..]
                .iter()
                .any(|part| matches!(&**part, ContentPart::Text { .. }))
        {
            return index;
        }
        let part = self.view.draft.remove(index);
        self.view.draft.push(part);
        let new_index = self.view.draft.len() - 1;
        for mapped in self.text_parts.values_mut() {
            if *mapped == index {
                *mapped = new_index;
            } else if *mapped > index {
                *mapped -= 1;
            }
        }
        new_index
    }

    async fn event(&mut self, event: AgentEvent) {
        // 文本与思考增量按节流发布，其余事件改变结构或状态，立即发布
        let is_delta = matches!(event, AgentEvent::Thought(_) | AgentEvent::Text { .. });
        let text_part = match &event {
            AgentEvent::Text { text_id, .. } => Some(self.text_part_index(text_id, true)),
            AgentEvent::Citation { text_id, .. } => Some(self.text_part_index(text_id, false)),
            _ => None,
        };
        match &event {
            AgentEvent::Thought(text) => {
                self.view.state = RunState::Thinking;
                let append_new = match self.view.draft.last() {
                    Some(part) => !matches!(&**part, ContentPart::Reasoning { .. }),
                    None => true,
                };
                if append_new {
                    self.view.draft.push(Arc::new(ContentPart::reasoning(text, None)));
                } else if let ContentPart::Reasoning { content, .. } =
                    Arc::make_mut(self.view.draft.last_mut().expect("checked above"))
                {
                    content.push_str(text);
                }
            }
            AgentEvent::Text { delta, .. } => {
                self.view.state = RunState::Responding;
                if let ContentPart::Text { text, .. } =
                    Arc::make_mut(&mut self.view.draft[text_part.unwrap()])
                {
                    text.push_str(delta);
                }
            }
            AgentEvent::Citation { annotation, .. } => {
                if let ContentPart::Text { annotations, .. } =
                    Arc::make_mut(&mut self.view.draft[text_part.unwrap()])
                    && !annotations.contains(annotation)
                {
                    annotations.push(annotation.clone());
                }
            }
            AgentEvent::ToolExecuting { name, .. } => {
                self.view.state = RunState::Tool(name.clone())
            }
            AgentEvent::WebSearchCall(call) => {
                if let Some(index) = self
                    .view
                    .draft
                    .iter()
                    .position(|part| matches!(&**part, ContentPart::WebSearchCall(existing) if existing.id == call.id))
                {
                    self.view.draft[index] = Arc::new(ContentPart::WebSearchCall(call.clone()));
                } else {
                    self.view.draft.push(Arc::new(ContentPart::WebSearchCall(call.clone())));
                }
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
            resolved.input_capabilities().validate_session(&session)?;
            session.set_model(resolved.full_spec, selected);
            AppState::persist_selection(
                &self.state_path,
                &session.current_model,
                session.current_variant.as_deref(),
            );
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

    async fn revert(&mut self, turn_index: usize) -> Result<(), String> {
        if !matches!(self.view.state, RunState::Idle) {
            return Err("Session is currently running another prompt".into());
        }
        let mut session = (*self.view.session).clone();
        if turn_index >= session.turns.len() {
            return Err("Turn not found".into());
        }
        if session.turns[turn_index].role != Role::User {
            return Err("Can only revert a user message".into());
        }
        session.truncate_turns(turn_index);
        if self.view.persisted {
            self.store
                .save_header(&session)
                .await
                .map_err(|e| e.to_string())?;
        }
        self.view.session = Arc::new(session);
        self.view.draft.clear();
        self.publish();
        Ok(())
    }

    async fn start(
        &mut self,
        prompt: &str,
        images: Vec<ContentPart>,
    ) -> Result<AgentEngine, String> {
        self.view.notices.clear();
        let tool_outputs = self
            .store
            .ensure_tool_outputs(&self.view.session.workspace_root)
            .await
            .map_err(|e| e.to_string())?;
        let engine = AgentEngine::new(&self.config, &self.view.session, tool_outputs)?;
        // 图片附件先经输入能力校验，再落盘，避免不支持模态的模型留下无效轮次
        for image in &images {
            if let ContentPart::Image { source, mime_type } = image {
                let mime = match source {
                    MediaSource::Base64 { mime_type, .. } => Some(mime_type.as_str()),
                    MediaSource::Url { .. } => mime_type.as_deref(),
                };
                match mime {
                    Some(mime) => engine
                        .inputs
                        .require(crate::model::MediaKind::Image, mime)?,
                    None => {
                        if !engine
                            .inputs
                            .modalities
                            .iter()
                            .any(|input| input == "image")
                        {
                            return Err(format!(
                                "Model '{}' does not support image input",
                                engine.inputs.model
                            ));
                        }
                    }
                }
            }
        }
        let generate = self.view.session.title.is_none();
        if !self.view.persisted {
            let mut session = (*self.view.session).clone();
            let workspace = session.workspace_root.clone();
            let scratch = self
                .store
                .ensure_scratch(&workspace)
                .await
                .map_err(|e| e.to_string())?;
            let (system, global_agents, project_agents) = tokio::task::spawn_blocking(move || {
                let system = crate::system::build_system_prompt(&workspace, &scratch);
                let global = crate::system::load_global_agents_md();
                let project = crate::system::load_project_agents_md(&workspace);
                (system, global, project)
            })
            .await
            .map_err(|e| e.to_string())?;

            session.append_turn(Turn::new(Role::System, vec![Message::text(system)]));

            let mut loaded_canonical = Vec::new();
            if let Some((path, content)) = global_agents {
                if let Ok(c) = path.canonicalize() {
                    loaded_canonical.push(c);
                }
                let reminder = crate::system::format_global_agents_reminder(&path, &content);
                session.append_turn(Turn::new(Role::User, vec![Message::text(reminder)]));
            }
            if let Some((path, content)) = project_agents {
                let is_dup = path
                    .canonicalize()
                    .ok()
                    .is_some_and(|c| loaded_canonical.contains(&c));
                if !is_dup {
                    let reminder = crate::system::format_project_agents_reminder(&path, &content);
                    session.append_turn(Turn::new(Role::User, vec![Message::text(reminder)]));
                }
            }
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
        let mut parts = images;
        parts.push(ContentPart::text(prompt));
        self.append(Turn::user_turn(Message::new(parts))).await?;
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
                    Some(Command::Submit { reply, .. }) | Some(Command::Revert { reply, .. }) => { let _ = reply.send(Err("Session is currently running another prompt".into())); }
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
        let worked_started = Instant::now();
        loop {
            self.text_parts.clear();
            self.view.state = RunState::Thinking;
            self.publish();
            let response_started = Instant::now();
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
            turn.elapsed_ms = Some(response_started.elapsed().as_millis() as u64);
            turn.worked_ms = calls
                .is_empty()
                .then(|| worked_started.elapsed().as_millis() as u64);
            turn.meta = response.meta;
            self.view.draft.clear();
            self.append(turn).await?;
            if calls.is_empty() {
                return Ok(true);
            }
            for call in &calls {
                if engine.tools.validate_call(call).is_ok() {
                    self.event(AgentEvent::ToolExecuting {
                        name: call.name.clone(),
                        args: call.arguments.clone(),
                    })
                    .await;
                }
            }
            let tools = engine.tools.clone();
            let mut set = tokio::task::JoinSet::new();
            for (index, call) in calls.iter().cloned().enumerate() {
                let tools = tools.clone();
                set.spawn(async move { (index, tools.execute_call(&call).await) });
            }
            let count = calls.len();
            let Some(results) = self
                .drive(
                    async move {
                        let mut results = vec![None; count];
                        while let Some(joined) = set.join_next().await {
                            let (index, result) = joined.expect("tool task panicked");
                            results[index] = Some(result);
                        }
                        results.into_iter().map(Option::unwrap).collect::<Vec<_>>()
                    },
                    commands,
                    &mut rx,
                )
                .await
            else {
                return Ok(false);
            };
            let mut on_demand_agents = Vec::new();
            for (call, mut result) in calls.into_iter().zip(results) {
                // Configure can run while tools are reading; validate before committing their media.
                if !result.attachments.is_empty() {
                    let inputs = self
                        .config
                        .resolve_model(&self.view.session.current_model, None)
                        .map_err(|error| error.to_string())?
                        .input_capabilities();
                    if let Some(error) = result
                        .attachments
                        .iter()
                        .find_map(|media| inputs.require(media.kind, &media.mime_type).err())
                    {
                        result = crate::model::ToolResult::error(&call.id, error);
                    }
                }
                self.event(AgentEvent::ToolFinished {
                    name: call.name.clone(),
                    result: result.content.clone(),
                    is_error: result.is_error,
                })
                .await;
                self.append(Turn::new(
                    Role::Tool,
                    vec![Message::tool_result(result.clone())],
                ))
                .await?;

                if call.name == "Read"
                    && !result.is_error
                    && let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    && let Some(path_str) = args.get("path").and_then(|p| p.as_str())
                {
                    let file_path = crate::tools::filesystem::resolve_path(
                        &self.view.session.workspace_root,
                        path_str,
                    );
                    if let Some(agents_path) = crate::system::find_directory_agents_md(
                        &self.view.session.workspace_root,
                        &file_path,
                    ) {
                        let ws_canonical = self
                            .view
                            .session
                            .workspace_root
                            .canonicalize()
                            .unwrap_or_else(|_| self.view.session.workspace_root.clone());
                        let agents_canonical = agents_path
                            .canonicalize()
                            .unwrap_or_else(|_| agents_path.clone());
                        let rel_path = agents_canonical
                            .strip_prefix(&ws_canonical)
                            .unwrap_or(&agents_path)
                            .to_string_lossy()
                            .replace('\\', "/");

                        let already_in_session = crate::system::session_contains_agents_md(
                            &self.view.session,
                            &rel_path,
                        );
                        let already_pending =
                            on_demand_agents.iter().any(|(p, _)| p == &agents_path);

                        if !already_in_session
                            && !already_pending
                            && let Ok(content) = std::fs::read_to_string(&agents_path)
                            && !content.trim().is_empty()
                        {
                            on_demand_agents.push((agents_path, content));
                        }
                    }
                }
            }

            for (agents_path, content) in on_demand_agents {
                let reminder = crate::system::format_directory_agents_reminder(
                    &self.view.session.workspace_root,
                    &agents_path,
                    &content,
                );
                self.append(Turn::new(Role::User, vec![Message::text(reminder)]))
                    .await?;
            }
        }
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
                    images,
                    events,
                    reply,
                } => {
                    let engine = match self.start(&prompt, images).await {
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
                Command::Revert { turn_index, reply } => {
                    let _ = reply.send(self.revert(turn_index).await);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Annotation, SearchStatus, WebSearchAction, WebSearchCall, WebSearchSource};
    use crate::storage::jsonl::JsonlSessionStore;
    use std::path::Path;

    fn event_task() -> (SessionTask, watch::Receiver<Arc<SessionView>>) {
        let workspace = std::env::temp_dir().join(format!("vva-events-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(JsonlSessionStore::new(&workspace).unwrap());
        let view = SessionView {
            revision: 0,
            session: Arc::new(Session::new(workspace.clone(), None)),
            draft: Vec::new(),
            notices: Vec::new(),
            state: RunState::Idle,
            persisted: false,
        };
        let (view_tx, view_rx) = watch::channel(Arc::new(view.clone()));
        let (commands, _rx) = mpsc::channel(1);
        let task = SessionTask {
            view,
            view_tx,
            store,
            config: Arc::new(AppConfig::default()),
            state_path: workspace.join("state.json"),
            commands: commands.downgrade(),
            dirty: false,
            cli: None,
            stop: None,
            notified: HashSet::new(),
            title_task: None,
            text_parts: HashMap::new(),
        };
        (task, view_rx)
    }

    #[tokio::test]
    async fn model_switch_checks_persisted_media_before_mutating_state() {
        let (mut task, _) = event_task();
        let root = task.view.session.workspace_root.clone();
        task.config = Arc::new(serde_json::from_value(serde_json::json!({
            "providers": {"test": {"models": {
                "vision": {"name": "Vision", "modalities": {"input": ["text", "image"]}},
                "text": {"name": "Text", "modalities": {"input": ["text"]}},
                "other": {"name": "Other", "type": "anthropic", "modalities": {"input": ["text", "image"]}}
            }}}
        })).unwrap());
        let session = Arc::make_mut(&mut task.view.session);
        session.set_model("test/vision", None);
        session.turns = crate::provider::common::media_tool_session().turns;
        task.store.save_header(&task.view.session).await.unwrap();
        task.view.session = Arc::new(
            task.store
                .load_session(&task.view.session.id)
                .await
                .unwrap()
                .unwrap(),
        );
        task.view.persisted = true;
        let before = task.view.session.clone();
        let revision = task.view.revision;
        let error = task
            .configure(Some("test/text".into()), None, None)
            .await
            .unwrap_err();
        assert!(error.contains("does not support image input"));
        assert_eq!(task.view.revision, revision);
        assert_eq!(task.view.session.current_model, "test/vision");
        assert!(!task.state_path.exists());
        assert_eq!(
            task.store
                .load_session(&before.id)
                .await
                .unwrap()
                .unwrap()
                .current_model,
            "test/vision"
        );
        task.configure(Some("test/other".into()), None, None)
            .await
            .unwrap();
        assert_eq!(task.view.session.current_model, "test/other");
        assert_eq!(task.view.session.turns, before.turns);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct BarrierTool(tokio::sync::Barrier);

    #[async_trait::async_trait]
    impl crate::tools::Tool for BarrierTool {
        fn name(&self) -> &str {
            "Barrier"
        }
        fn description(&self) -> &str {
            "Concurrency test"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"label": {"type": "string"}}})
        }
        async fn execute(&self, arguments: &str) -> Result<String, String> {
            self.0.wait().await;
            Ok(arguments.into())
        }
    }

    struct ParallelProvider(std::sync::atomic::AtomicBool);

    #[async_trait::async_trait]
    impl crate::provider::Provider for ParallelProvider {
        fn name(&self) -> &str {
            "parallel-test"
        }
        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            Default::default()
        }
        async fn complete(
            &self,
            _: &Session,
            _: &[crate::model::ToolDefinition],
            _: &crate::provider::CompletionOptions,
        ) -> Result<crate::provider::ProviderResponse, crate::provider::ProviderError> {
            let messages = if self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
                vec![Message::text("done")]
            } else {
                vec![Message::new(
                    ["first", "second"]
                        .iter()
                        .map(|label| {
                            ContentPart::ToolCall(crate::model::ToolCall::new(
                                *label,
                                "Barrier",
                                serde_json::json!({"label": label}).to_string(),
                            ))
                        })
                        .collect(),
                )]
            };
            Ok(crate::provider::ProviderResponse {
                messages,
                usage: None,
                meta: Default::default(),
            })
        }
        async fn complete_stream(
            &self,
            session: &Session,
            tools: &[crate::model::ToolDefinition],
            options: &crate::provider::CompletionOptions,
            _: Option<mpsc::Sender<AgentEvent>>,
        ) -> Result<crate::provider::ProviderResponse, crate::provider::ProviderError> {
            self.complete(session, tools, options).await
        }
    }

    #[tokio::test]
    async fn parallel_tool_calls_run_concurrently_and_preserve_order() {
        let (mut task, _) = event_task();
        let root = task.view.session.workspace_root.clone();
        task.store.save_header(&task.view.session).await.unwrap();
        let mut tools = crate::tools::ToolRegistry::new();
        tools.register(Arc::new(BarrierTool(tokio::sync::Barrier::new(2))));
        let engine = AgentEngine {
            provider: Arc::new(ParallelProvider(std::sync::atomic::AtomicBool::new(false))),
            tools,
            options: crate::provider::CompletionOptions {
                enable_web_search: false,
                ..Default::default()
            },
            inputs: Default::default(),
        };
        let (_tx, mut rx) = mpsc::channel(1);
        let finished = tokio::time::timeout(Duration::from_secs(2), task.execute(engine, &mut rx))
            .await
            .unwrap()
            .unwrap();
        assert!(finished);
        let ids: Vec<_> = task
            .view
            .session
            .turns
            .iter()
            .flat_map(|turn| &turn.messages)
            .flat_map(|msg| &msg.parts)
            .filter_map(|part| match part {
                ContentPart::ToolResult(result) => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["first", "second"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn late_citations_target_text_id_and_deduplicate_only_exact_spans() {
        let (mut task, mut view_rx) = event_task();
        task.event(AgentEvent::Text {
            text_id: "first".into(),
            delta: "Alpha ".into(),
        })
        .await;
        assert!(!view_rx.has_changed().unwrap());
        task.publish_throttled();
        assert!(view_rx.has_changed().unwrap());
        assert_eq!(
            view_rx.borrow_and_update().draft,
            vec![Arc::new(ContentPart::text("Alpha "))]
        );

        let search = WebSearchCall::completed("search", "sources", Vec::new());
        task.event(AgentEvent::WebSearchCall(search.clone())).await;
        task.event(AgentEvent::Text {
            text_id: "second".into(),
            delta: "Other text".into(),
        })
        .await;
        task.event(AgentEvent::Text {
            text_id: "first".into(),
            delta: "Beta".into(),
        })
        .await;

        let first = Annotation::UrlCitation {
            url: "https://example.com/source".into(),
            title: "Source".into(),
            start_index: 0,
            end_index: 5,
        };
        let second = Annotation::UrlCitation {
            url: "https://example.com/source".into(),
            title: "Source".into(),
            start_index: 6,
            end_index: 10,
        };
        for annotation in [&first, &first, &second, &second] {
            view_rx.borrow_and_update();
            task.event(AgentEvent::Citation {
                text_id: "first".into(),
                annotation: annotation.clone(),
            })
            .await;
            assert!(view_rx.has_changed().unwrap());
        }

        let published = view_rx.borrow_and_update().clone();
        assert_eq!(
            published.draft,
            vec![
                Arc::new(ContentPart::Text {
                    text: "Alpha Beta".into(),
                    annotations: vec![first, second],
                }),
                Arc::new(ContentPart::WebSearchCall(search)),
                Arc::new(ContentPart::text("Other text")),
            ]
        );
        assert!(matches!(published.state, RunState::Responding));
        assert!(published.session.turns.is_empty());
        assert!(!published.persisted);
        std::fs::remove_dir_all(&published.session.workspace_root).unwrap();
    }

    #[tokio::test]
    async fn continued_text_after_search_moves_behind_work() {
        let (mut task, mut view_rx) = event_task();
        task.event(AgentEvent::Thought("plan".into())).await;
        task.event(AgentEvent::Text {
            text_id: "answer".into(),
            delta: "early ".into(),
        })
        .await;
        let search = WebSearchCall::completed("search", "query", Vec::new());
        task.event(AgentEvent::WebSearchCall(search.clone())).await;
        task.event(AgentEvent::Thought("review".into())).await;
        task.event(AgentEvent::Text {
            text_id: "answer".into(),
            delta: "later".into(),
        })
        .await;
        task.publish_throttled();
        let published = view_rx.borrow_and_update().clone();
        assert_eq!(
            published.draft,
            vec![
                Arc::new(ContentPart::reasoning("plan", None)),
                Arc::new(ContentPart::WebSearchCall(search)),
                Arc::new(ContentPart::reasoning("review", None)),
                Arc::new(ContentPart::text("early later")),
            ]
        );
        std::fs::remove_dir_all(&published.session.workspace_root).unwrap();
    }

    #[tokio::test]
    async fn search_snapshots_update_in_place_and_publish_before_completion() {
        let (mut task, mut view_rx) = event_task();
        let pending = WebSearchCall {
            action: WebSearchAction::Search,
            id: "search".into(),
            query: String::new(),
            status: SearchStatus::Pending,
            sources: Vec::new(),
        };
        task.event(AgentEvent::WebSearchCall(pending.clone())).await;
        assert!(view_rx.has_changed().unwrap());
        let initial = view_rx.borrow_and_update().clone();
        assert_eq!(
            initial.draft,
            vec![Arc::new(ContentPart::WebSearchCall(pending.clone()))]
        );

        task.event(AgentEvent::Text {
            text_id: "answer".into(),
            delta: "Draft answer".into(),
        })
        .await;
        let other = WebSearchCall::completed("other-search", "other query", Vec::new());
        task.event(AgentEvent::WebSearchCall(other.clone())).await;

        let searching = WebSearchCall {
            query: "updated query".into(),
            status: SearchStatus::Searching,
            ..pending
        };
        let completed = WebSearchCall::completed(
            "search",
            "final query",
            vec![WebSearchSource::new(
                "https://example.com/source",
                "Source",
                Some("Result snippet".into()),
            )],
        );
        for snapshot in [searching, completed.clone(), completed] {
            let revision = view_rx.borrow_and_update().revision;
            task.event(AgentEvent::WebSearchCall(snapshot.clone()))
                .await;
            assert!(view_rx.has_changed().unwrap());
            let published = view_rx.borrow_and_update().clone();
            assert_eq!(published.revision, revision + 1);
            assert_eq!(
                published.draft,
                vec![
                    Arc::new(ContentPart::WebSearchCall(snapshot)),
                    Arc::new(ContentPart::text("Draft answer")),
                    Arc::new(ContentPart::WebSearchCall(other.clone())),
                ]
            );
            assert!(matches!(published.state, RunState::Responding));
            assert!(published.session.turns.is_empty());
            assert!(!published.persisted);
        }
        assert_eq!(initial.draft.len(), 1);
        std::fs::remove_dir_all(&task.view.session.workspace_root).unwrap();
    }

    fn last_model_manager(dir: &Path) -> SessionManager {
        let store = Arc::new(JsonlSessionStore::new(dir).unwrap());
        let mut config = AppConfig::default();
        config.settings.default = "openai/default_model".into();
        config.providers.insert(
            "openai".into(),
            crate::config::ProviderConfig {
                api_key: "test".into(),
                ..Default::default()
            },
        );
        SessionManager::new(store, config, dir, dir.join("state.json"))
    }

    #[tokio::test]
    async fn new_session_uses_last_selected_model() {
        let dir = std::env::temp_dir().join(format!("vva-last-model-{}", uuid::Uuid::new_v4()));
        let mgr = last_model_manager(&dir);
        let first = mgr
            .create_session("openai/first".into(), Some("high".into()), None, true)
            .await
            .unwrap();
        first
            .configure(Some("openai/second".into()), Some(Some("low".into())), None)
            .await
            .unwrap();

        let restarted = last_model_manager(&dir);
        let next = restarted
            .open_session(None, None, None, None)
            .await
            .unwrap();
        let snap = next.snapshot();
        assert_eq!(snap.session.current_model, "openai/second");
        assert_eq!(snap.session.current_variant.as_deref(), Some("low"));

        let explicit = restarted
            .open_session(None, Some("openai/explicit".into()), None, None)
            .await
            .unwrap();
        assert_eq!(explicit.snapshot().session.current_model, "openai/explicit");

        let alpha = restarted
            .create_session("openai/alpha".into(), None, None, true)
            .await
            .unwrap();
        restarted
            .create_session("openai/beta".into(), None, None, true)
            .await
            .unwrap();
        restarted.get_or_load(&alpha.id).await.unwrap();
        let after_switch = last_model_manager(&dir)
            .open_session(None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            after_switch.snapshot().session.current_model,
            "openai/alpha"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn seeded_session(dir: &Path) -> (SessionManager, Arc<ManagedSession>, String) {
        let store = Arc::new(JsonlSessionStore::new(dir).unwrap());
        let mut session = Session::with_model(
            dir,
            Some("Origin".into()),
            "openai/default_model",
            Some("high".into()),
            true,
        );
        session.append_turn(Turn::new(Role::System, vec![Message::text("sys")]));
        session.append_turn(Turn::user_turn(Message::text("first")));
        session.append_turn(Turn::assistant_turn(vec![Message::text("a1")]));
        session.append_turn(Turn::user_turn(Message::text("second")));
        session.append_turn(Turn::assistant_turn(vec![Message::text("a2")]));
        let id = session.id.clone();
        store.save_header(&session).await.unwrap();
        let mut config = AppConfig::default();
        config.settings.default = "openai/default_model".into();
        config.providers.insert(
            "openai".into(),
            crate::config::ProviderConfig {
                api_key: "test".into(),
                ..Default::default()
            },
        );
        let mgr = SessionManager::new(store, config, dir, dir.join("state.json"));
        let handle = mgr.get_or_load(&id).await.unwrap();
        (mgr, handle, id)
    }

    #[tokio::test]
    async fn revert_truncates_from_user_turn_and_persists() {
        let dir = std::env::temp_dir().join(format!("vva-revert-{}", uuid::Uuid::new_v4()));
        let (mgr, handle, id) = seeded_session(&dir).await;
        handle.revert(3).await.unwrap();
        let snap = handle.snapshot();
        assert_eq!(snap.session.turns.len(), 3);
        assert_eq!(snap.session.turns[2].text_content(), "a1");
        let loaded = mgr.store().load_session(&id).await.unwrap().unwrap();
        assert_eq!(loaded.turns.len(), 3);
        assert_eq!(loaded.turns[1].text_content(), "first");
        assert!(handle.revert(0).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fork_copies_prefix_and_leaves_origin() {
        let dir = std::env::temp_dir().join(format!("vva-fork-{}", uuid::Uuid::new_v4()));
        let (mgr, handle, id) = seeded_session(&dir).await;
        let forked = mgr
            .fork_session(&handle, 3, "Branched".into())
            .await
            .unwrap();
        let origin = handle.snapshot();
        let branch = forked.snapshot();
        assert_eq!(origin.session.turns.len(), 5);
        assert_eq!(origin.session.title.as_deref(), Some("Origin"));
        assert_eq!(branch.session.turns.len(), 3);
        assert_eq!(branch.session.title.as_deref(), Some("Branched"));
        assert_eq!(branch.session.current_model, "openai/default_model");
        assert_eq!(branch.session.current_variant.as_deref(), Some("high"));
        assert_ne!(forked.id, id);
        let loaded = mgr.store().load_session(&forked.id).await.unwrap().unwrap();
        assert_eq!(loaded.turns.len(), 3);
        assert_eq!(loaded.title.as_deref(), Some("Branched"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn new_session_loads_project_agents_md() {
        let dir = std::env::temp_dir().join(format!("vva-agents-load-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "project instructions").unwrap();

        let (mut task, _rx) = event_task();
        let mut config = AppConfig::default();
        config.settings.default = "openai/test_model".into();
        config.providers.insert(
            "openai".into(),
            crate::config::ProviderConfig {
                api_key: "test".into(),
                ..Default::default()
            },
        );
        task.config = Arc::new(config);
        task.view.session = Arc::new(Session::with_model(
            &dir,
            None,
            "openai/test_model",
            None,
            false,
        ));
        task.store = Arc::new(JsonlSessionStore::new(&dir).unwrap());

        task.start("hello", Vec::new()).await.unwrap();
        let snap = task.view.session;

        // 断言相对结构而非固定下标：存在全局 ~/.vva/AGENTS.md 的机器会多出一个 Global 轮次
        assert_eq!(snap.turns.first().map(|t| t.role), Some(Role::System));
        assert_eq!(snap.turns.last().map(|t| t.role), Some(Role::User));
        assert_eq!(snap.turns.last().unwrap().text_content(), "hello");
        let project = snap.turns[..snap.turns.len() - 1]
            .iter()
            .filter_map(|t| crate::system::parse_agents_reminder(&t.text_content()))
            .find(|r| r.kind == crate::system::AgentsReminderKind::Project)
            .expect("project AGENTS.md injected before user prompt");
        assert!(project.content.contains("project instructions"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
