use super::*;

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
    pub(super) notified: HashSet<String>,
    pub(super) title_task: Option<tokio::task::JoinHandle<()>>,
    /// 响应内文本标识到草稿部件的映射，供增量与引用共同定位。
    pub(super) text_parts: HashMap<String, usize>,
    pub(super) ask_user_tx: mpsc::Sender<crate::tools::ask_user::AskUserRequest>,
    pub(super) ask_user_rx: mpsc::Receiver<crate::tools::ask_user::AskUserRequest>,
    pub(super) pending_user_question: Option<crate::tools::ask_user::AskUserRequest>,
}

/// 提交前校验附件：图片先过输入能力，File 校验 MIME 一致性
fn validate_attachments(engine: &AgentEngine, images: &[ContentPart]) -> Result<(), String> {
    for image in images {
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
        } else if let ContentPart::File {
            source: MediaSource::Base64 { mime_type, .. },
            mime_type: declared_mime,
            ..
        } = image
        {
            if declared_mime != mime_type {
                return Err("File MIME type does not match its source".into());
            }
            engine
                .inputs
                .require(crate::model::MediaKind::Pdf, mime_type)?;
        }
    }
    Ok(())
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
    let file_path = crate::tools::filesystem::resolve_path(&session.workspace_root, path_str);
    let agents_path = crate::system::find_directory_agents_md(&session.workspace_root, &file_path)?;
    let ws_canonical = session
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| session.workspace_root.clone());
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
            AgentEvent::ImageGenerationCall(call) => {
                if let Some(index) = self
                    .view
                    .draft
                    .iter()
                    .position(|part| matches!(&**part, ContentPart::ImageGenerationCall(existing) if existing.id == call.id))
                {
                    self.view.draft[index] =
                        Arc::new(ContentPart::ImageGenerationCall(call.clone()));
                } else {
                    self.view
                        .draft
                        .push(Arc::new(ContentPart::ImageGenerationCall(call.clone())));
                }
            }
            AgentEvent::ToolFinished { .. } => {}
            AgentEvent::AskUser(_) => {}
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

    pub(super) async fn append(&mut self, turn: Turn) -> Result<(), String> {
        self.store
            .append_turn(&self.view.session.id, &turn)
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
        search: Option<bool>,
    ) -> Result<(), String> {
        if model.is_none() && variant.is_none() && search.is_none() {
            return Ok(());
        }
        let mut session = (*self.view.session).clone();
        if model.is_some() || variant.is_some() {
            let previous_model = session.current_model.clone();
            let target = model.as_deref().unwrap_or(&session.current_model);
            let selected = variant.unwrap_or_else(|| session.current_variant.clone());
            let resolved = self
                .config
                .resolve_model(target, selected.as_deref())
                .map_err(|e| e.to_string())?;
            resolved.input_capabilities().validate_session(&session)?;
            session.set_model(resolved.full_spec, selected);
            // 切换模型即能力/亲和状态变迁：允许通知重新发布（状态变迁发射语义）
            if previous_model != session.current_model {
                self.notified.clear();
            }
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
        self.replace_session(session);
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
        self.replace_session(session);
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
        self.view.draft.clear();
        self.replace_session(session);
        Ok(())
    }

    pub(super) async fn start(
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
        let skills = crate::skills::discover_skills(&self.view.session.workspace_root);
        let engine = AgentEngine::new(
            &self.config,
            &self.view.session,
            tool_outputs,
            self.hashline.clone(),
            self.ask_user_tx.clone(),
            skills.clone(),
        )?;
        // 图片附件先经输入能力校验，再落盘，避免不支持模态的模型留下无效轮次
        validate_attachments(&engine, &images)?;
        let generate = self.view.session.title.is_none();
        if !self.view.persisted {
            self.persist_new_session(&skills, prompt, generate).await?;
        }
        self.view.draft.clear();
        let mut parts = images;
        parts.push(ContentPart::text(prompt));
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
        if !skills.is_empty() {
            let reminder = crate::system::format_skills_reminder(skills);
            session.append_turn(Turn::new(Role::User, vec![Message::text(reminder)]));
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
        Ok(())
    }

    /// 后台小模型生成正式标题，完成后经 Title 命令回写会话
    fn spawn_title_task(&mut self, prompt: &str) {
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
                Some(request) = self.ask_user_rx.recv() => self.receive_user_question(request).await,
                _ = publish_tick.tick() => self.publish_throttled(),
                command = commands.recv() => match command {
                    Some(Command::Configure { model, variant, search, reply }) => {
                        self.handle_configure(model, variant, search, reply).await;
                    }
                    Some(Command::Title(title)) => self.handle_title(title).await,
                    Some(Command::Submit { reply, .. }) | Some(Command::Revert { reply, .. }) => { let _ = reply.send(Err("Session is currently running another prompt".into())); }
                    Some(Command::AnswerUser { request_id, answers, reply }) => {
                        let _ = reply.send(self.answer_user(request_id, answers).await);
                    }
                    Some(Command::Stop { delete, reply }) => {
                        self.cancel_pending_user_question();
                        self.stop_request = Some((delete, reply));
                        self.publish_throttled();
                        return None;
                    }
                    None => { self.publish_throttled(); return None; }
                }
            }
        }
    }

    /// Configure 命令在运行中 (drive) 与空闲 (run) 两条路径共用同一处理
    async fn handle_configure(
        &mut self,
        model: Option<String>,
        variant: Option<Option<String>>,
        search: Option<bool>,
        reply: Reply,
    ) {
        let _ = reply.send(self.configure(model, variant, search).await);
    }

    /// Title 命令在运行中 (drive) 与空闲 (run) 两条路径共用同一处理
    async fn handle_title(&mut self, title: String) {
        if let Err(error) = self.title(title).await {
            self.event(AgentEvent::Notice(error)).await;
        }
    }

    pub(super) async fn execute(
        &mut self,
        engine: AgentEngine,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<bool, String> {
        for notice in engine.notices.clone() {
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
            let on_demand_agents = self.commit_tool_results(calls, results).await?;
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

    pub(super) async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
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
                    if let Some((delete, reply)) = self.stop_request.take()
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
                    self.handle_configure(model, variant, search, reply).await;
                }
                Command::Title(title) => self.handle_title(title).await,
                Command::Revert { turn_index, reply } => {
                    let _ = reply.send(self.revert(turn_index).await);
                }
                Command::AnswerUser { reply, .. } => {
                    let _ = reply.send(Err("No user question is waiting for an answer".into()));
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
