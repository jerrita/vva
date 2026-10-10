use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnOutcome {
    Completed,
    Aborted,
}

/// 会话任务的外部依赖
pub(super) struct TaskDeps {
    pub store: Arc<dyn SessionStore>,
    pub config: Arc<AppConfig>,
    pub state_path: PathBuf,
    /// 会话编排接口（子代理派生与跨会话投递）
    pub broker: Arc<dyn crate::tools::agent::SessionBroker>,
    /// 会话工具工厂（缺省不追加工具）
    pub tool_factory: Option<SessionToolFactory>,
}

pub(super) struct SessionTask {
    pub(super) hashline: Arc<crate::tools::hashline::HashlineSession>,
    pub(super) view: SessionView,
    pub(super) view_tx: watch::Sender<Arc<SessionView>>,
    pub(super) store: Arc<dyn SessionStore>,
    pub(super) config: Arc<AppConfig>,
    pub(super) state_path: PathBuf,
    pub(super) commands: mpsc::WeakSender<Command>,
    /// 流式增量只置脏，由 drive 的节流 tick 或结构性变化触发发布
    pub(super) dirty: bool,
    pub(super) cli: Option<mpsc::Sender<AgentEvent>>,
    /// 待处理的停止请求 (是否删除, 应答)，当前轮次结束后执行
    pub(super) stop_request: Option<(bool, Reply)>,
    pub(super) title_task: Option<tokio::task::JoinHandle<()>>,
    /// 响应内文本标识到草稿部件的映射，供增量与引用共同定位。
    pub(super) text_parts: HashMap<String, usize>,
    pub(super) ask_user_tx: mpsc::Sender<crate::tools::ask_user::AskUserRequest>,
    pub(super) ask_user_rx: mpsc::Receiver<crate::tools::ask_user::AskUserRequest>,
    pub(super) pending_user_question: Option<crate::tools::ask_user::AskUserRequest>,
    /// 会话级后台任务注册表（Shell run_in_background），随会话结束回收
    pub(super) tasks: Arc<crate::tools::background::BackgroundTasks>,
    /// 已完成、尚未注入会话的后台任务 id（轮次间隙注入）
    pub(super) task_completions: Vec<String>,
    /// 已完成、尚未注入会话的后台子代理 session id（轮次间隙注入最终消息）
    pub(super) subagent_completions: Vec<String>,
    /// 运行中到达的程序化消息及其独立完成回执
    pub(super) pending_deliveries: Vec<PendingDelivery>,
    /// 已完成轮次计数通道：每次 finish_turn 递增，供外部等待轮次结束
    pub(super) turn_tx: watch::Sender<CompletedTurn>,
    pub(super) turn_start: usize,
    /// 会话编排接口（子代理派生与跨会话投递）
    pub(super) broker: Arc<dyn crate::tools::agent::SessionBroker>,
    pub(super) tool_factory: Option<SessionToolFactory>,
}

/// 单个附件的输入能力校验：图片过输入能力（MIME 或模态声明），File 校验 MIME 一致性与 PDF 能力。
/// 返回 Some(错误文案) 表示当前模型不能接收该附件。
fn attachment_error(engine: &AgentEngine, part: &ContentPart) -> Option<String> {
    if let ContentPart::Image { source, mime_type } = part {
        let mime = match source {
            MediaSource::Base64 { mime_type, .. } => Some(mime_type.as_str()),
            MediaSource::Url { .. } => mime_type.as_deref(),
        };
        return match mime {
            Some(mime) => engine
                .inputs
                .require(crate::model::MediaKind::Image, mime)
                .err(),
            None => {
                if engine
                    .inputs
                    .modalities
                    .iter()
                    .any(|input| input == "image")
                {
                    None
                } else {
                    Some(format!(
                        "Model '{}' does not support image input",
                        engine.inputs.model
                    ))
                }
            }
        };
    }
    if let ContentPart::File {
        source: MediaSource::Base64 { mime_type, .. },
        mime_type: declared_mime,
        ..
    } = part
    {
        if declared_mime != mime_type {
            return Some("File MIME type does not match its source".into());
        }
        return engine
            .inputs
            .require(crate::model::MediaKind::Pdf, mime_type)
            .err();
    }
    None
}

/// 提交前校验上传附件：避免不支持模态的模型留下无效轮次
fn validate_attachments(engine: &AgentEngine, parts: &[ContentPart]) -> Result<(), String> {
    parts
        .iter()
        .find_map(|part| attachment_error(engine, part))
        .map_or(Ok(()), Err)
}

/// Read 工具命中工作空间内子目录 AGENTS.md 时返回待注入的 (路径, 正文)；
/// 会话已载入同路径或本轮已排队的文件跳过。
fn collect_on_demand_agents(
    session: &Session,
    call: &crate::model::ToolCall,
    result: &crate::model::ToolResult,
    pending: &[(PathBuf, String)],
) -> Option<(PathBuf, String)> {
    if call.name != "Read" || result.is_error {
        return None;
    }
    let args = serde_json::from_str::<serde_json::Value>(&call.arguments).ok()?;
    let path_str = args.get("path").and_then(|p| p.as_str())?;
    let file_path =
        crate::tools::filesystem::resolve_path(&session.header.workspace_root, path_str);
    let agents_path =
        crate::system::find_directory_agents_md(&session.header.workspace_root, &file_path)?;
    let ws_canonical = session
        .header
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| session.header.workspace_root.clone());
    let agents_canonical = agents_path
        .canonicalize()
        .unwrap_or_else(|_| agents_path.clone());
    let rel_path = agents_canonical
        .strip_prefix(&ws_canonical)
        .unwrap_or(&agents_path)
        .to_string_lossy()
        .replace('\\', "/");

    let already_in_session = crate::system::session_contains_agents_md(session, &rel_path);
    let already_pending = pending.iter().any(|(p, _)| p == &agents_path);
    if already_in_session || already_pending {
        return None;
    }
    let content = std::fs::read_to_string(&agents_path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    Some((agents_path, content))
}

impl SessionTask {
    /// 构造任务及其发布通道：返回 (任务, 视图订阅, 已完成轮次订阅)；
    /// 后台任务完成经 `commands` 弱引用回投，通道饱和或会话已退出时丢弃，
    /// 模型可经 TaskOutput 轮询自愈。
    pub(super) fn new(
        session: Session,
        persisted: bool,
        deps: TaskDeps,
        commands: mpsc::WeakSender<Command>,
    ) -> (
        Self,
        watch::Receiver<Arc<SessionView>>,
        watch::Receiver<CompletedTurn>,
    ) {
        let view = SessionView::new(session, persisted);
        let (view_tx, view_rx) = watch::channel(Arc::new(view.clone()));
        let (ask_user_tx, ask_user_rx) = mpsc::channel(4);
        let (turn_tx, turns) = watch::channel(CompletedTurn::default());
        let notify = commands.clone();
        let tasks = Arc::new(crate::tools::background::BackgroundTasks::new(Arc::new(
            move |task_id| {
                if let Some(tx) = notify.upgrade() {
                    let _ = tx.try_send(Command::TaskFinished { task_id });
                }
            },
        )));
        let task = Self {
            view,
            view_tx,
            store: deps.store,
            config: deps.config,
            hashline: crate::tools::hashline::HashlineSession::new(),
            state_path: deps.state_path,
            commands,
            dirty: false,
            cli: None,
            stop_request: None,
            title_task: None,
            text_parts: HashMap::new(),
            ask_user_tx,
            ask_user_rx,
            pending_user_question: None,
            tasks,
            task_completions: Vec::new(),
            subagent_completions: Vec::new(),
            pending_deliveries: Vec::new(),
            turn_tx,
            turn_start: 0,
            broker: deps.broker,
            tool_factory: deps.tool_factory,
        };
        (task, view_rx, turns)
    }

    pub(super) fn publish(&mut self) {
        self.dirty = false;
        self.view.revision += 1;
        self.view_tx.send_replace(Arc::new(self.view.clone()));
    }

    /// 流式增量期间累积改动，由节流点统一发布，避免每个 token 复制整份草稿
    pub(super) fn publish_throttled(&mut self) {
        if self.dirty {
            self.publish();
        }
    }

    /// 用修改后的会话替换视图并发布快照
    fn replace_session(&mut self, session: Session) {
        self.view.session = Arc::new(session);
        self.publish();
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

    /// 同身份的草稿部件原位替换，否则追加
    fn upsert_draft_part(&mut self, part: ContentPart, is_same: impl Fn(&ContentPart) -> bool) {
        let part = Arc::new(part);
        match self
            .view
            .draft
            .iter()
            .position(|existing| is_same(existing))
        {
            Some(index) => self.view.draft[index] = part,
            None => self.view.draft.push(part),
        }
    }

    pub(super) async fn event(&mut self, event: AgentEvent) {
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
                match self.view.draft.last_mut().map(Arc::make_mut) {
                    Some(ContentPart::Reasoning { content, .. }) => content.push_str(text),
                    _ => self.view.draft.push(Arc::new(ContentPart::reasoning(text, None))),
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
            AgentEvent::WebSearchCall(call) => self.upsert_draft_part(
                ContentPart::WebSearchCall(call.clone()),
                |part| matches!(part, ContentPart::WebSearchCall(existing) if existing.id == call.id),
            ),
            AgentEvent::ImageGenerationCall(call) => self.upsert_draft_part(
                ContentPart::ImageGenerationCall(call.clone()),
                |part| matches!(part, ContentPart::ImageGenerationCall(existing) if existing.id == call.id),
            ),
            AgentEvent::ToolFinished { .. } | AgentEvent::AskUser(_) | AgentEvent::Finished => {}
            AgentEvent::Notice(text) => {
                self.view
                    .notices
                    .push(Notice::new(text.clone(), NoticeKind::Session));
            }
            AgentEvent::Capabilities(texts) => {
                // 能力提示整体替换，文案未变的沿用原提示 (保持稳定 id)
                let (mut previous, mut kept): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut self.view.notices)
                        .into_iter()
                        .partition(|notice| matches!(notice.kind, NoticeKind::Capability));
                for text in texts {
                    let notice = match previous.iter().position(|notice| &notice.text == text) {
                        Some(index) => previous.swap_remove(index),
                        None => Notice::new(text.clone(), NoticeKind::Capability),
                    };
                    kept.push(notice);
                }
                self.view.notices = kept;
            }
            AgentEvent::TaskCompleted { task_id, text } => {
                if let Some(notice) = self.view.notices.iter_mut().find(|notice| {
                    matches!(&notice.kind, NoticeKind::Task { task_id: id } if id == task_id)
                }) {
                    notice.text.clone_from(text);
                } else {
                    self.view.notices.push(Notice::new(
                        text.clone(),
                        NoticeKind::Task { task_id: task_id.clone() },
                    ));
                }
            }
            AgentEvent::Error(text) => {
                self.view.draft.clear();
                self.record_outcome(text.clone(), false);
            }
            AgentEvent::Aborted => {
                // 未落盘的草稿随取消一并丢弃，界面与存储保持一致
                self.view.draft.clear();
                self.record_outcome("Turn aborted.".into(), true);
            }
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

    pub(super) async fn append(&mut self, turn: Turn) -> Result<(), String> {
        self.store
            .append_turn(&self.view.session.header.id, &turn)
            .await
            .map_err(|e| e.to_string())?;
        Arc::make_mut(&mut self.view.session).append_turn(turn);
        self.publish();
        Ok(())
    }

    pub(super) async fn configure(
        &mut self,
        model: Option<String>,
        variant: Option<Option<String>>,
        toggles: ToggleOverrides,
    ) -> Result<(), String> {
        if model.is_none() && variant.is_none() && toggles.is_empty() {
            return Ok(());
        }
        let mut session = (*self.view.session).clone();
        let selection_changed = model.is_some() || variant.is_some();
        if selection_changed {
            let target = model.as_deref().unwrap_or(&session.header.current_model);
            let selected = variant.unwrap_or_else(|| session.header.current_variant.clone());
            let resolved = self
                .config
                .resolve_model(target, selected.as_deref())
                .map_err(|e| e.to_string())?;
            resolved.input_capabilities().validate_session(&session)?;
            session.set_model(resolved.full_spec, selected);
        }
        if !toggles.is_empty() {
            session.set_toggles(toggles.apply(session.toggles()));
        }
        let skills = crate::skills::discover_skills(&session.header.workspace_root);
        let engine = self.build_engine_for(&session, skills).await?;
        if self.view.persisted {
            self.store
                .save_header(&session)
                .await
                .map_err(|e| e.to_string())?;
        }
        if selection_changed {
            AppState::persist_selection(
                &self.state_path,
                &session.header.current_model,
                session.header.current_variant.as_deref(),
            );
        }
        self.replace_session(session);
        self.view.external_web = engine.external_web;
        self.event(AgentEvent::Capabilities(engine.notices)).await;
        Ok(())
    }

    async fn save_header_if_persisted(&self, session: &Session) -> Result<(), String> {
        if !self.view.persisted {
            return Ok(());
        }
        self.store
            .save_header(session)
            .await
            .map_err(|e| e.to_string())
    }

    async fn title(&mut self, title: String) -> Result<(), String> {
        if title.is_empty() {
            return Ok(());
        }
        let mut session = (*self.view.session).clone();
        session.header.title = Some(title);
        self.store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;
        self.replace_session(session);
        Ok(())
    }

    async fn revert(&mut self, turn_index: usize) -> Result<(), String> {
        if !matches!(self.view.state, RunState::Idle) {
            return Err(BUSY.into());
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
        self.view.draft.clear();
        self.view.notices.retain(|notice| match &notice.kind {
            NoticeKind::Turn { turn_id, .. } => session.turns.iter().any(|turn| &turn.id == turn_id),
            NoticeKind::Task { task_id } => session.turns.iter().flat_map(|turn| &turn.messages)
                .flat_map(|message| &message.parts).any(|part| matches!(part, ContentPart::ToolResult(result) if result.background_task_id.as_ref() == Some(task_id))),
            _ => true,
        });
        self.replace_session(session);
        Ok(())
    }

    /// 组装本轮流次的引擎：工具输出目录就绪后装配注册表与供应商
    async fn build_engine_for(
        &self,
        session: &Session,
        skills: Vec<crate::skills::Skill>,
    ) -> Result<AgentEngine, String> {
        let tool_outputs = self
            .store
            .ensure_tool_outputs(&session.header.workspace_root)
            .await
            .map_err(|e| e.to_string())?;
        let agents = crate::agents::discover_agents(&session.header.workspace_root);
        AgentEngine::new(
            &self.config,
            session,
            EngineDeps {
                tool_outputs,
                hashline: self.hashline.clone(),
                ask_user_requests: self.ask_user_tx.clone(),
                background: self.tasks.clone(),
                skills,
                agents,
                broker: self.broker.clone(),
                extra_tools: self
                    .tool_factory
                    .as_ref()
                    .map(|factory| factory(&session.header))
                    .unwrap_or_default(),
            },
        )
    }

    /// 以当前会话组装引擎 (重新发现技能)
    async fn build_engine(&self) -> Result<AgentEngine, String> {
        let skills = crate::skills::discover_skills(&self.view.session.header.workspace_root);
        self.build_engine_for(&self.view.session, skills).await
    }

    pub(super) async fn start(
        &mut self,
        prompt: &str,
        images: Vec<ContentPart>,
    ) -> Result<AgentEngine, String> {
        let skills = crate::skills::discover_skills(&self.view.session.header.workspace_root);
        let engine = self
            .build_engine_for(&self.view.session, skills.clone())
            .await?;
        // @ 引用展开：文本文件转 FileRef，图片/PDF 转原生附件（模型不支持时降级为文本注记）；
        // 路径不可读发 Notice 不阻断提交
        let workspace = self.view.session.header.workspace_root.clone();
        let mut mention_parts = Vec::new();
        for token in crate::harness::mention::extract_mention_paths(prompt) {
            match crate::harness::mention::load_mention(&workspace, &token).await {
                Ok(part) => {
                    let part = if attachment_error(&engine, &part).is_some() {
                        crate::harness::mention::capability_note(&token)
                    } else {
                        part
                    };
                    mention_parts.push(part);
                }
                Err(reason) => {
                    self.event(AgentEvent::Notice(format!("@{token}: {reason}")))
                        .await;
                }
            }
        }
        // 上传附件先经输入能力校验，再落盘，避免不支持模态的模型留下无效轮次
        validate_attachments(&engine, &images)?;
        let generate = self.view.session.header.title.is_none();
        if !self.view.persisted {
            self.persist_new_session(&skills, prompt, generate).await?;
        }
        self.view.draft.clear();
        let mut parts = images;
        parts.push(ContentPart::text(prompt));
        parts.extend(mention_parts);
        self.append(Turn::user_turn(Message::new(parts))).await?;
        // /<skill_name> 斜杠调用：user turn 保留原文，技能正文以独立 reminder 轮次注入
        if let Some(skill) = crate::skills::match_invocation(prompt, &skills) {
            let reminder = crate::system::format_skill_load_reminder(skill);
            self.append(Turn::new(Role::User, vec![Message::text(reminder)]))
                .await?;
        }
        self.view.state = RunState::Thinking;
        self.publish();
        if generate {
            self.spawn_title_task(prompt);
        }
        Ok(engine)
    }

    /// 首次落盘：组装 System Prompt / AGENTS.md / 技能清单轮次并写入 Header
    async fn persist_new_session(
        &mut self,
        skills: &[crate::skills::Skill],
        prompt: &str,
        generate: bool,
    ) -> Result<(), String> {
        let mut session = (*self.view.session).clone();
        let workspace = session.header.workspace_root.clone();
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

        // 子代理会话：agent 定义的角色指令追加到基础 System Prompt
        let agent_def = session
            .header
            .agent
            .as_deref()
            .map(|name| {
                crate::agents::discover_agents(&session.header.workspace_root)
                    .into_iter()
                    .find(|def| def.name == name)
                    .ok_or_else(|| format!("Agent definition '{name}' is unavailable"))
            })
            .transpose()?;
        let system = match agent_def
            .as_ref()
            .and_then(|def| def.system_prompt.as_deref())
        {
            Some(extra) => format!("{system}\n\n{extra}"),
            None => system,
        };
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
        // 子代理工具白名单未含 Skill 时不注入技能清单（指令与能力保持一致）
        let skill_allowed = agent_def
            .as_ref()
            .is_none_or(|def| def.tools.iter().any(|tool| tool == "Skill"));
        if !skills.is_empty() && skill_allowed {
            let reminder = crate::system::format_skills_reminder(skills);
            session.append_turn(Turn::new(Role::User, vec![Message::text(reminder)]));
        }
        if generate {
            session.header.title = Some(crate::model::provisional_title(prompt));
        }
        self.store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;
        self.view.session = Arc::new(session);
        self.view.persisted = true;
        Ok(())
    }

    /// 后台小模型生成正式标题，完成后经 Title 命令回写会话
    fn spawn_title_task(&mut self, prompt: &str) {
        let config = self.config.clone();
        let model = self.view.session.header.current_model.clone();
        let prompt = prompt.to_string();
        let commands = self.commands.clone();
        self.title_task = Some(tokio::spawn(async move {
            match generate_title(&config, &model, &prompt).await {
                Ok(title) => {
                    if let Some(tx) = commands.upgrade() {
                        let _ = tx.send(Command::Shared(SharedCommand::Title(title))).await;
                    }
                }
                Err(error) => tracing::warn!("Title generation failed: {error}"),
            }
        }));
    }

    /// 后台任务完成：id 入队等待轮次间隙注入完整内容（background-task reminder）；
    /// 完成状态关联任务身份，忙闲路径共用。
    pub(super) async fn queue_task_completion(&mut self, task_id: String) {
        let notice = self
            .tasks
            .snapshot(&task_id)
            .map(|snapshot| {
                format!(
                    "Background task {} {}",
                    snapshot.id,
                    crate::tools::background::status_line(&snapshot.status)
                )
            })
            .unwrap_or_else(|| format!("Background task {task_id} finished"));
        self.event(AgentEvent::TaskCompleted {
            task_id: task_id.clone(),
            text: notice,
        })
        .await;
        self.task_completions.push(task_id);
    }

    /// 将已完成后台任务的完整通知作为用户轮次注入会话（下一次模型调用可见）
    pub(super) async fn drain_task_completions(&mut self) -> Result<(), String> {
        for task_id in std::mem::take(&mut self.task_completions) {
            match self.tasks.snapshot(&task_id) {
                Some(snapshot) => {
                    let text = crate::tools::background::format_notification(&snapshot);
                    self.append(Turn::new(Role::User, vec![Message::text(text)]))
                        .await?;
                }
                None => tracing::warn!("Background task {task_id} vanished before notification"),
            }
        }
        Ok(())
    }

    /// 后台子代理完成：入队等待轮次间隙注入最终消息；
    /// 完成状态关联子会话身份，忙闲路径共用。
    pub(super) async fn queue_subagent_completion(&mut self, session_id: String) {
        self.event(AgentEvent::TaskCompleted {
            task_id: session_id.clone(),
            text: format!("Background agent {session_id} finished"),
        })
        .await;
        self.subagent_completions.push(session_id);
    }

    /// 将后台子代理的最终消息通知作为用户轮次注入会话（下一次模型调用可见）
    pub(super) async fn drain_subagent_completions(&mut self) -> Result<(), String> {
        for session_id in std::mem::take(&mut self.subagent_completions) {
            match self.broker.session_report(&session_id).await {
                Ok(report) => {
                    let text = crate::tools::agent::format_subagent_reminder(&report);
                    self.append(Turn::new(Role::User, vec![Message::text(text)]))
                        .await?;
                }
                Err(error) => {
                    tracing::warn!("Subagent {session_id} report failed: {error}");
                }
            }
        }
        Ok(())
    }

    /// 网络与工具等待期间处理设置和取消；磁盘提交在本任务中顺序完成。
    /// 流式增量以 PUBLISH_INTERVAL 为节奏发布，慢于终端重绘频率即无可见差异。
    pub(super) async fn drive<T>(
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
                Some(request) = self.ask_user_rx.recv() => self.receive_user_question(request).await,
                _ = publish_tick.tick() => self.publish_throttled(),
                command = commands.recv() => {
                    let Some(command) = command else {
                        self.publish_throttled();
                        return None;
                    };
                    match command {
                        Command::Shared(command) => self.handle_shared(command).await,
                        Command::TaskFinished { task_id } => {
                            self.queue_task_completion(task_id).await;
                        }
                        Command::SubagentFinished { session_id } => {
                            self.queue_subagent_completion(session_id).await;
                        }
                        Command::Deliver { delivery, reply } => {
                            // 程序化消息不拒绝：入队待轮次结束后处理
                            self.pending_deliveries.push(delivery);
                            let _ = reply.send(Ok(()));
                        }
                        Command::Submit { reply, .. } | Command::Revert { reply, .. } => {
                            let _ = reply.send(Err(BUSY.into()));
                        }
                        Command::Stop { delete, reply } => {
                            self.cancel_pending_user_question();
                            self.stop_request = Some((delete, reply));
                            self.publish_throttled();
                            return None;
                        }
                    }
                }
            }
        }
    }

    /// 运行中 (drive) 与空闲 (run) 语义一致的命令
    async fn handle_shared(&mut self, command: SharedCommand) {
        match command {
            SharedCommand::Configure {
                model,
                variant,
                toggles,
                reply,
            } => {
                let _ = reply.send(self.configure(model, variant, toggles).await);
            }
            SharedCommand::Title(title) => {
                if let Err(error) = self.title(title).await {
                    self.event(AgentEvent::Notice(error)).await;
                }
            }
            SharedCommand::Supersede { by, reply } => {
                let mut session = (*self.view.session).clone();
                session.header.superseded_by = Some(by);
                let result = self.save_header_if_persisted(&session).await;
                if result.is_ok() {
                    self.replace_session(session);
                }
                let _ = reply.send(result);
            }
            SharedCommand::DismissNotice { id, reply } => {
                self.dismiss_notice(&id);
                let _ = reply.send(Ok(()));
            }
            SharedCommand::AnswerUser {
                request_id,
                answers,
                reply,
            } => {
                let _ = reply.send(self.answer_user(request_id, answers).await);
            }
        }
    }

    fn record_outcome(&mut self, text: String, aborted: bool) {
        let kind = self
            .view
            .session
            .turns
            .last()
            .map_or(NoticeKind::Session, |turn| NoticeKind::Turn {
                turn_id: turn.id.clone(),
                aborted,
            });
        self.view.notices.push(Notice::new(text, kind));
    }

    /// 通过稳定身份关闭提示；重复关闭保持幂等。
    pub(super) fn dismiss_notice(&mut self, id: &str) {
        self.view
            .notices
            .retain(|notice| notice.id != id || matches!(notice.kind, NoticeKind::Capability));
        self.publish();
    }

    /// 单个用户轮次：反复「模型响应 → 并行工具」直至模型不再调用工具
    pub(super) async fn execute(
        &mut self,
        engine: AgentEngine,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<TurnOutcome, String> {
        self.event(AgentEvent::Capabilities(engine.notices.clone()))
            .await;
        self.view.external_web = engine.external_web.clone();
        let definitions = engine.tools.definitions();
        let worked_started = Instant::now();
        loop {
            self.drain_task_completions().await?;
            self.drain_subagent_completions().await?;
            self.text_parts.clear();
            self.view.state = RunState::Thinking;
            self.publish();
            let response_started = Instant::now();
            let context = self.view.session.clone();
            let (tx, mut rx) = mpsc::channel(64);
            let request =
                engine
                    .provider
                    .complete_stream(&context, &definitions, &engine.options, Some(tx));
            let Some(response) = self.drive(request, commands, &mut rx).await else {
                return Ok(TurnOutcome::Aborted);
            };
            let response = response.map_err(|e| e.to_string())?;
            let calls: Vec<_> = response
                .messages
                .iter()
                .flat_map(|m| &m.parts)
                .filter_map(|p| match p {
                    ContentPart::ToolCall(call) => Some(call.clone()),
                    _ => None,
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
                return Ok(TurnOutcome::Completed);
            }
            let Some(results) = self.run_tools(&engine, &calls, commands, &mut rx).await else {
                // 中止：为未决 tool_call 合成错误结果，保持 tool_call / tool_result 配对完整；
                // 前台子代理随父会话本轮中止联动停止
                let results = calls
                    .iter()
                    .map(|call| {
                        crate::model::ToolResult::error(&call.id, "Tool execution aborted by user")
                    })
                    .collect();
                self.commit_tool_results(calls, results).await?;
                self.broker
                    .abort_children(&self.view.session.header.id)
                    .await;
                return Ok(TurnOutcome::Aborted);
            };
            for (agents_path, content) in self.commit_tool_results(calls, results).await? {
                let reminder = crate::system::format_directory_agents_reminder(
                    &self.view.session.header.workspace_root,
                    &agents_path,
                    &content,
                );
                self.append(Turn::new(Role::User, vec![Message::text(reminder)]))
                    .await?;
            }
        }
    }

    /// 并行执行一批工具调用，结果按调用顺序返回；被 Stop 中断时返回 None
    async fn run_tools(
        &mut self,
        engine: &AgentEngine,
        calls: &[crate::model::ToolCall],
        commands: &mut mpsc::Receiver<Command>,
        events: &mut mpsc::Receiver<AgentEvent>,
    ) -> Option<Vec<crate::model::ToolResult>> {
        for call in calls {
            if engine.tools.validate_call(call).is_ok() {
                self.event(AgentEvent::ToolExecuting {
                    name: call.name.clone(),
                    args: call.arguments.clone(),
                })
                .await;
            }
        }
        let mut set = tokio::task::JoinSet::new();
        for (index, call) in calls.iter().cloned().enumerate() {
            let tools = engine.tools.clone();
            set.spawn(async move { (index, tools.execute_call(&call).await) });
        }
        let mut results = vec![None; calls.len()];
        let collect = async {
            while let Some(joined) = set.join_next().await {
                let (index, result) = joined.expect("tool task panicked");
                results[index] = Some(result);
            }
        };
        self.drive(collect, commands, events).await?;
        Some(results.into_iter().map(Option::unwrap).collect())
    }

    /// 逐个提交工具结果：附件复验 → ToolFinished 事件 → Tool 轮次落盘，
    /// 返回本轮待注入的子目录 AGENTS.md 清单。
    async fn commit_tool_results(
        &mut self,
        calls: Vec<crate::model::ToolCall>,
        results: Vec<crate::model::ToolResult>,
    ) -> Result<Vec<(PathBuf, String)>, String> {
        let mut on_demand_agents = Vec::new();
        for (call, mut result) in calls.into_iter().zip(results) {
            // Configure can run while tools are reading; validate before committing their media.
            if !result.attachments.is_empty() {
                let inputs = self
                    .config
                    .resolve_model(&self.view.session.header.current_model, None)
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

            if let Some(agents) =
                collect_on_demand_agents(&self.view.session, &call, &result, &on_demand_agents)
            {
                on_demand_agents.push(agents);
            }
        }
        Ok(on_demand_agents)
    }

    async fn receive_user_question(&mut self, request: crate::tools::ask_user::AskUserRequest) {
        if self.pending_user_question.is_some() {
            let _ = request.response.send(Err(
                "Only one AskUser request can be active at a time".into()
            ));
            return;
        }
        let prompt = request.prompt.clone();
        self.view.pending_user_question = Some(prompt.clone());
        self.pending_user_question = Some(request);
        self.event(AgentEvent::AskUser(prompt)).await;
    }

    async fn answer_user(
        &mut self,
        request_id: String,
        answers: Vec<UserAnswer>,
    ) -> Result<(), String> {
        let Some(request) = self.pending_user_question.take() else {
            return Err("No user question is waiting for an answer".into());
        };
        if request.prompt.id != request_id {
            self.pending_user_question = Some(request);
            return Err("User question ID does not match the pending request".into());
        }
        if answers.is_empty() {
            self.view.pending_user_question = None;
            self.publish();
            let _ = request
                .response
                .send(Err("User canceled the question".into()));
            return Ok(());
        }
        let result = crate::tools::ask_user::encode_answers(&request.prompt, &answers);
        if result.is_err() {
            self.pending_user_question = Some(request);
            return result.map(|_| ());
        }
        self.view.pending_user_question = None;
        self.publish();
        request
            .response
            .send(result)
            .map_err(|_| "AskUser tool stopped before receiving the answer".to_string())
    }

    fn cancel_pending_user_question(&mut self) {
        if let Some(request) = self.pending_user_question.take() {
            let _ = request
                .response
                .send(Err("User canceled the question".into()));
            self.view.pending_user_question = None;
            self.publish();
        }
    }

    async fn stop(&mut self, delete: bool, reply: Reply) -> bool {
        self.cancel_deliveries("Session stopped");
        let result = if delete && self.view.persisted {
            self.store
                .delete_session(&self.view.session.header.id)
                .await
                .map_err(|e| e.to_string())
        } else {
            Ok(())
        };
        let close = delete && result.is_ok();
        let _ = reply.send(result);
        close
    }

    /// 连续执行轮次：后台任务与后台子代理在轮次间隙完成时自动续跑，直到队列清空或轮次终止。
    /// 用户中止与供应商错误不自动续跑，队列留给下次 Submit 注入。
    async fn run_turns(
        &mut self,
        mut engine: AgentEngine,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<TurnOutcome, String> {
        self.turn_start = self.view.session.turns.len();
        loop {
            let outcome = self.execute(engine, commands).await?;
            if outcome == TurnOutcome::Aborted
                || (self.task_completions.is_empty() && self.subagent_completions.is_empty())
            {
                return Ok(outcome);
            }
            engine = self.build_engine().await?;
        }
    }

    /// 轮次收尾：状态复位、终态事件、CLI 通道释放与停止请求处理；
    /// 返回是否终止会话任务
    pub(super) async fn finish_turn(&mut self, result: Result<TurnOutcome, String>) -> bool {
        let error = match &result {
            Ok(TurnOutcome::Completed) => None,
            Ok(TurnOutcome::Aborted) => Some("Turn aborted".to_string()),
            Err(error) => Some(error.clone()),
        };
        if let Some(error) = &error {
            self.cancel_deliveries(error);
        }
        let text = if error.is_none() {
            self.view.session.turns[self.turn_start..]
                .iter()
                .rev()
                .find(|turn| turn.role == Role::Assistant)
                .map(Turn::text_content)
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.view.state = RunState::Idle;
        match result {
            Ok(TurnOutcome::Completed) => self.event(AgentEvent::Finished).await,
            Ok(TurnOutcome::Aborted) => self.event(AgentEvent::Aborted).await,
            Err(error) => self.event(AgentEvent::Error(error)).await,
        }
        self.cli = None;
        self.touch_activity().await;
        // 已完成轮次计数递增，唤醒等待本轮结束的外部订阅者（Agent / WaitSession）
        let seq = self.turn_tx.borrow().seq + 1;
        self.turn_tx
            .send_replace(CompletedTurn { seq, text, error });
        if let Some((delete, reply)) = self.stop_request.take() {
            return self.stop(delete, reply).await;
        }
        false
    }

    /// agent space 会话每个轮次结束刷新 `last_activity`（TTL 清理依据）；
    /// 重写 header 失败只告警，不影响轮次结果
    async fn touch_activity(&mut self) {
        if !self.view.persisted || !self.view.session.header.agent_managed() {
            return;
        }
        let mut session = (*self.view.session).clone();
        session.header.last_activity = Some(chrono::Utc::now());
        match self.save_header_if_persisted(&session).await {
            Ok(()) => self.replace_session(session),
            Err(error) => tracing::warn!("Failed to persist last_activity: {error}"),
        }
    }

    fn cancel_deliveries(&mut self, error: &str) {
        for delivery in std::mem::take(&mut self.pending_deliveries) {
            let _ = delivery.completion.send(Err(error.into()));
        }
    }

    /// 同一轮次间隙的投递合并处理，每个投递者获得该轮固定的完成报告。
    pub(super) async fn run_pending_deliveries(
        &mut self,
        commands: &mut mpsc::Receiver<Command>,
    ) -> bool {
        while !self.pending_deliveries.is_empty() {
            let deliveries = std::mem::take(&mut self.pending_deliveries);
            let text = deliveries
                .iter()
                .map(|delivery| delivery.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let engine = match self.start(&text, Vec::new()).await {
                Ok(engine) => engine,
                Err(error) => {
                    for delivery in deliveries {
                        let _ = delivery.completion.send(Err(error.clone()));
                    }
                    self.cancel_deliveries(&error);
                    self.event(AgentEvent::Notice(error)).await;
                    return false;
                }
            };
            let result = self.run_turns(engine, commands).await;
            let successful = matches!(result, Ok(TurnOutcome::Completed));
            let close = self.finish_turn(result).await;
            let report = self
                .turn_tx
                .borrow()
                .subagent_report(self.view.session.header.id.clone(), &self.view.session);
            for delivery in deliveries {
                let _ = delivery.completion.send(Ok(report.clone()));
            }
            if close || !successful {
                return close;
            }
        }
        false
    }

    /// 空闲时完成通知已入队：自动续跑，execute 首轮注入通知；返回真表示终止会话任务
    async fn run_completion_turn(&mut self, commands: &mut mpsc::Receiver<Command>) -> bool {
        match self.build_engine().await {
            Ok(engine) => {
                let result = self.run_turns(engine, commands).await;
                self.finish_turn(result).await || self.run_pending_deliveries(commands).await
            }
            // 通知保留在队列，下次 Submit 注入
            Err(error) => {
                self.event(AgentEvent::Notice(error)).await;
                false
            }
        }
    }

    pub(super) async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        while let Some(command) = commands.recv().await {
            match command {
                Command::Shared(command) => self.handle_shared(command).await,
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
                    let result = self.run_turns(engine, &mut commands).await;
                    if self.finish_turn(result).await
                        || self.run_pending_deliveries(&mut commands).await
                    {
                        break;
                    }
                }
                Command::Deliver { delivery, reply } => {
                    // 空闲时到达的程序化消息：立即作为下一轮执行
                    let _ = reply.send(Ok(()));
                    self.pending_deliveries.push(delivery);
                    if self.run_pending_deliveries(&mut commands).await {
                        break;
                    }
                }
                Command::TaskFinished { task_id } => {
                    self.queue_task_completion(task_id).await;
                    if self.run_completion_turn(&mut commands).await {
                        break;
                    }
                }
                Command::SubagentFinished { session_id } => {
                    self.queue_subagent_completion(session_id).await;
                    if self.run_completion_turn(&mut commands).await {
                        break;
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
        self.tasks.shutdown();
    }
}
