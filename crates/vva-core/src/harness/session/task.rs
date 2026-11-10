use super::*;
use crate::tools::names;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Aborted,
}

/// 会话任务的外部依赖
pub struct TaskDeps {
    pub store: Arc<dyn SessionStore>,
    pub setup: Arc<dyn crate::harness::SessionSetup>,
}

pub struct PendingSubagentCompletion {
    task: Option<crate::model::BackgroundTask>,
    reminder: String,
}

pub struct SessionTask {
    #[cfg(feature = "filesystem-tools")]
    pub hashline: Arc<crate::tools::hashline::HashlineSession>,
    pub view: SessionView,
    pub view_tx: watch::Sender<Arc<SessionView>>,
    pub store: Arc<dyn SessionStore>,
    pub setup: Arc<dyn crate::harness::SessionSetup>,
    pub commands: mpsc::WeakSender<Command>,
    /// 流式增量只置脏，由 drive 的节流 tick 或结构性变化触发发布
    pub dirty: bool,
    pub cli: Option<mpsc::Sender<AgentEvent>>,
    /// 待处理的停止请求 (是否删除, 应答)，当前轮次结束后执行
    pub stop_request: Option<(bool, Reply)>,
    pub title_task: Option<tokio::task::JoinHandle<()>>,
    /// 响应内文本标识到草稿部件的映射，供增量与引用共同定位。
    pub text_parts: HashMap<String, usize>,
    pub ask_user_tx: mpsc::Sender<crate::tools::ask_user::AskUserRequest>,
    pub ask_user_rx: mpsc::Receiver<crate::tools::ask_user::AskUserRequest>,
    pub pending_user_question: Option<crate::tools::ask_user::AskUserRequest>,
    /// 会话级 Shell 输出采集与合并唤醒。
    pub shell_outputs: Arc<crate::tools::shell_output::ShellOutputs>,
    output_pending: bool,
    last_output_publish: tokio::time::Instant,
    pub shell_calls: std::collections::HashSet<String>,
    /// 会话级后台任务注册表（Shell run_in_background），随会话结束回收
    pub tasks: Arc<crate::tools::background::BackgroundTasks>,
    /// 已完成、尚未注入会话的后台任务 id（轮次间隙注入）
    pub task_completions: Vec<String>,
    /// 已完成、尚未注入会话的后台子代理报告（捕获完成瞬间的最终消息）
    pub subagent_completions: Vec<PendingSubagentCompletion>,
    /// 运行中到达的程序化消息及其独立完成回执
    pub pending_deliveries: Vec<PendingDelivery>,
    /// 已执行投递的回执，保留到异步工作及其续跑完成。
    pub active_deliveries: Vec<PendingDelivery>,
    pub async_count: usize,
    pub auto_resume: bool,
    /// 已完成轮次计数通道：每次 finish_turn 递增，供外部等待轮次结束
    pub turn_tx: watch::Sender<CompletedTurn>,
    pub turn_start: usize,
    /// 运行中到达、待轮次间隙追加的提示（只追加，不触发轮次）
    pub pending_notes: Vec<String>,
}

/// 提交前校验上传附件：避免不支持模态的模型留下无效轮次
pub fn validate_attachments(engine: &ExecutionPlan, parts: &[ContentPart]) -> Result<(), String> {
    for part in parts {
        engine.inputs.validate_part(part)?;
    }
    Ok(())
}

impl SessionTask {
    /// 构造任务及其发布通道：返回 (任务, 视图订阅, 已完成轮次订阅)；
    /// 后台任务完成经 `commands` 弱引用可靠回投；会话关闭后停止投递。
    pub fn new(
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
                    tokio::spawn(async move {
                        let _ = tx.send(Command::TaskFinished { task_id }).await;
                    });
                }
            },
        )));
        let task = Self {
            shell_calls: shell_call_ids(&view.session),
            shell_outputs: Arc::default(),
            output_pending: false,
            last_output_publish: tokio::time::Instant::now(),
            view,
            view_tx,
            store: deps.store,
            setup: deps.setup,
            #[cfg(feature = "filesystem-tools")]
            hashline: crate::tools::hashline::HashlineSession::new(),
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
            active_deliveries: Vec::new(),
            async_count: 0,
            auto_resume: true,
            turn_tx,
            turn_start: 0,
            pending_notes: Vec::new(),
        };
        (task, view_rx, turns)
    }

    async fn flush_shell_outputs(&mut self) {
        self.output_pending = false;
        self.last_output_publish = tokio::time::Instant::now();
        let updates = self.shell_outputs.drain();
        let mut changed = false;
        for (call_id, output) in updates {
            if self.shell_calls.contains(&call_id) {
                // 所有展示变动统一通过 AgentEvent 投影。
                self.event(AgentEvent::ShellOutputUpdated { call_id, output })
                    .await;
                changed = true;
            }
        }
        if changed {
            self.publish_throttled();
        }
    }

    pub fn publish(&mut self) {
        self.dirty = false;
        self.view.revision += 1;
        self.view_tx.send_replace(Arc::new(self.view.clone()));
    }

    fn pending_background(&self) -> usize {
        self.tasks.pending_count() + self.async_count + self.subagent_completions.len()
    }

    /// 流式增量期间累积改动，由节流点统一发布，避免每个 token 复制整份草稿
    pub fn publish_throttled(&mut self) {
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

    pub async fn event(&mut self, event: AgentEvent) {
        // 文本与思考增量按节流发布，其余事件改变结构或状态，立即发布
        let is_delta = matches!(
            event,
            AgentEvent::Thought(_)
                | AgentEvent::Text { .. }
                | AgentEvent::ShellOutputUpdated { .. }
        );
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
            AgentEvent::ShellOutputUpdated { call_id, output } => {
                Arc::make_mut(&mut self.view.shell_outputs).insert(call_id.clone(), output.clone());
            }
            AgentEvent::BackgroundTaskUpdated(task) => {
                crate::model::update_background_task(Arc::make_mut(&mut self.view.background_tasks), task.clone());
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
        if matches!(event, AgentEvent::ShellOutputUpdated { .. }) {
            return;
        }
        if let Some(tx) = &self.cli {
            let _ = tx.send(event).await;
        }
    }

    pub async fn append(&mut self, turn: Turn) -> Result<(), String> {
        self.store
            .append_turn(&self.view.session.header.id, &turn)
            .await
            .map_err(|e| e.to_string())?;
        let tasks: Vec<_> = turn
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter_map(|part| match part {
                ContentPart::ToolResult(result) => result.task.clone(),
                _ => None,
            })
            .chain(turn.background_task.clone())
            .collect();
        Arc::make_mut(&mut self.view.session).append_turn(turn);
        if tasks.is_empty() {
            self.publish();
        } else {
            for task in tasks {
                self.event(AgentEvent::BackgroundTaskUpdated(task)).await;
            }
        }
        Ok(())
    }

    pub async fn configure(
        &mut self,
        model: Option<String>,
        variant: Option<Option<String>>,
    ) -> Result<(), String> {
        let mut session = (*self.view.session).clone();
        let selection_changed = model.is_some() || variant.is_some();
        if selection_changed {
            self.setup.configure(&mut session, model, variant)?;
        }
        let engine = self.build_engine_for(&session).await?;
        if self.view.persisted {
            self.store
                .save_header(&session)
                .await
                .map_err(|e| e.to_string())?;
        }
        if selection_changed {
            self.setup.selection_saved(&session)?;
        }
        self.replace_session(session);
        self.view.external_web = engine.external_web;
        self.event(AgentEvent::Capabilities(engine.notices)).await;
        Ok(())
    }

    /// 修改 header：已落盘会话先重写 header，成功后替换视图
    async fn update_header(
        &mut self,
        update: impl FnOnce(&mut crate::model::SessionHeader),
    ) -> Result<(), String> {
        let mut session = (*self.view.session).clone();
        update(&mut session.header);
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
            NoticeKind::Turn { turn_id, .. } => {
                session.turns.iter().any(|turn| &turn.id == turn_id)
            }
            _ => true,
        });
        let mut tasks = crate::model::background_tasks(&session);
        for (id, task) in &mut tasks {
            if let Some(current) = self.view.background_tasks.get(id) {
                *task = current.clone();
            }
        }
        self.view.background_tasks = Arc::new(tasks);
        self.shell_calls = shell_call_ids(&session);
        Arc::make_mut(&mut self.view.shell_outputs).retain(|id, _| self.shell_calls.contains(id));
        self.replace_session(session);
        Ok(())
    }

    fn services(&self) -> crate::harness::SessionServices {
        crate::harness::SessionServices {
            #[cfg(feature = "filesystem-tools")]
            hashline: self.hashline.clone(),
            ask_user_requests: self.ask_user_tx.clone(),
            background: self.tasks.clone(),
            shell_outputs: self.shell_outputs.clone(),
        }
    }

    async fn build_engine_for(&self, session: &Session) -> Result<ExecutionPlan, String> {
        let plan = self.setup.plan(session, &self.services()).await?;
        plan.inputs.validate_session(session)?;
        Ok(plan)
    }

    pub async fn build_engine(&self) -> Result<ExecutionPlan, String> {
        self.build_engine_for(&self.view.session).await
    }

    pub async fn start(
        &mut self,
        prompt: &str,
        images: Vec<ContentPart>,
    ) -> Result<ExecutionPlan, String> {
        self.append_input(prompt, images, false).await
    }

    pub async fn append_input(
        &mut self,
        prompt: &str,
        images: Vec<ContentPart>,
        steering: bool,
    ) -> Result<ExecutionPlan, String> {
        let mut parts = images;
        parts.push(ContentPart::text(prompt));
        let prepared = self
            .setup
            .prepare_input(
                &self.view.session,
                Message::new(parts),
                !self.view.persisted,
            )
            .await?;
        let engine = self.build_engine().await?;
        for message in prepared
            .initial
            .iter()
            .flat_map(|turn| &turn.messages)
            .chain(&prepared.before)
            .chain(std::iter::once(&prepared.message))
            .chain(&prepared.after)
        {
            validate_attachments(&engine, &message.parts)?;
        }
        for notice in prepared.notices {
            self.event(AgentEvent::Notice(notice)).await;
        }
        let generate = self.view.session.header.title.is_none();
        if !self.view.persisted {
            let mut session = (*self.view.session).clone();
            for turn in prepared.initial {
                session.append_turn(turn);
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
        }
        if !steering {
            self.view.draft.clear();
        }
        for message in prepared.before {
            self.append(Turn::user_turn(message)).await?;
        }
        let mut turn = Turn::user_turn(prepared.message);
        turn.steering = steering;
        self.append(turn).await?;
        for message in prepared.after {
            self.append(Turn::user_turn(message)).await?;
        }
        if !steering {
            self.auto_resume = true;
        }
        self.view.state = RunState::Thinking;
        self.publish();
        if generate && !steering {
            self.spawn_title_task(prompt);
        }
        Ok(engine)
    }

    fn spawn_title_task(&mut self, prompt: &str) {
        let setup = self.setup.clone();
        let session = self.view.session.clone();
        let prompt = prompt.to_string();
        let commands = self.commands.clone();
        self.title_task = Some(tokio::spawn(async move {
            match setup.title(&session, &prompt).await {
                Ok(Some(title)) => {
                    if let Some(tx) = commands.upgrade() {
                        let _ = tx.send(Command::Shared(SharedCommand::Title(title))).await;
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!("Title generation failed: {error}"),
            }
        }));
    }

    /// 后台任务完成：原位发布状态，id 入队等待安全轮隙持久化并投递模型。
    pub async fn queue_task_completion(&mut self, task_id: String) {
        self.flush_shell_outputs().await;
        if let Some(snapshot) = self.tasks.snapshot(&task_id) {
            self.event(AgentEvent::BackgroundTaskUpdated(snapshot.task()))
                .await;
        }
        if !self.task_completions.contains(&task_id) {
            self.task_completions.push(task_id);
        }
    }

    /// 将已完成后台任务的完整通知作为用户轮次注入会话（下一次模型调用可见）
    pub async fn drain_task_completions(&mut self) -> Result<(), String> {
        while let Some(task_id) = self.task_completions.first().cloned() {
            match self.tasks.snapshot(&task_id) {
                Some(snapshot) => {
                    let text = crate::tools::background::format_notification(&snapshot);
                    let mut turn = Turn::new(Role::User, vec![Message::text(text)]);
                    turn.background_task = Some(snapshot.task());
                    self.append(turn).await?;
                }
                None => tracing::warn!("Background task {task_id} vanished before notification"),
            }
            self.tasks.acknowledge(&task_id);
            self.task_completions.remove(0);
        }
        Ok(())
    }

    /// 追加提示（只落盘，不起轮次）：空闲直接追加，运行中入队等轮次间隙
    pub async fn note(&mut self, text: String) -> Result<(), String> {
        if !self.view.persisted {
            return Err("Session has no persisted history to append a note to".into());
        }
        self.append(Turn::new(Role::User, vec![Message::text(text)]))
            .await
    }

    /// 将运行中排队的提示依序追加到历史末尾
    pub async fn drain_notes(&mut self) -> Result<(), String> {
        for text in std::mem::take(&mut self.pending_notes) {
            tracing::debug!("冲刷排队提示: {} 字符", text.chars().count());
            self.note(text).await?;
        }
        Ok(())
    }

    /// 登记与 watcher 创建在会话内一起完成，工具取消不会留下无回投的计数。
    pub fn watch_session(
        &mut self,
        session_id: String,
        receipt: super::handle::DeliveryReceipt,
        background: bool,
    ) {
        self.async_count += 1;
        self.publish();
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let result = receipt
                .await
                .map_err(|_| "Session is closed".to_string())
                .and_then(|result| result);
            let report = result.unwrap_or_else(|error| crate::tools::agent::SubagentReport {
                session_id,
                agent: String::new(),
                title: String::new(),
                turn_seq: 0,
                text: String::new(),
                error: Some(error),
            });
            if let Some(commands) = commands.upgrade() {
                let _ = commands
                    .send(Command::SessionFinished { report, background })
                    .await;
            }
        });
    }

    /// 会话结果在投递时已经固定，首轮后台委派同时更新原工具块。
    pub async fn queue_session_completion(
        &mut self,
        report: crate::tools::agent::SubagentReport,
        background: bool,
    ) {
        let task = if background {
            let mut task = crate::tools::agent::subagent_task(&report);
            if self
                .view
                .background_tasks
                .get(&report.session_id)
                .is_some_and(|previous| {
                    previous.status == crate::model::BackgroundTaskStatus::Stopped
                })
            {
                task.status = crate::model::BackgroundTaskStatus::Stopped;
            }
            self.event(AgentEvent::BackgroundTaskUpdated(task.clone()))
                .await;
            Some(task)
        } else {
            None
        };
        let reminder = crate::tools::agent::format_session_reminder(&report, background);
        self.subagent_completions
            .push(PendingSubagentCompletion { task, reminder });
    }

    /// 在工具结果完整配对后追加会话结果，随后交给模型。
    pub async fn drain_subagent_completions(&mut self) -> Result<(), String> {
        while let Some(pending) = self.subagent_completions.first() {
            let mut turn = Turn::new(Role::User, vec![Message::text(&pending.reminder)]);
            turn.background_task = pending.task.clone();
            self.append(turn).await?;
            self.subagent_completions.remove(0);
        }
        Ok(())
    }

    /// 网络与工具等待期间处理设置和取消；磁盘提交在本任务中顺序完成。
    /// 流式增量以 PUBLISH_INTERVAL 为节奏发布，慢于终端重绘频率即无可见差异。
    pub async fn drive<T>(
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
                    if !self.ready_commands(commands).await { return None; }
                    self.publish_throttled();
                    return Some(result);
                }
                Some(event) = events.recv() => self.event(event).await,
                Some(request) = self.ask_user_rx.recv() => self.receive_user_question(request).await,
                _ = self.shell_outputs.notified(), if !self.output_pending => self.output_pending = true,
                _ = publish_tick.tick() => {
                    if self.output_pending && self.last_output_publish.elapsed() >= crate::tools::shell_output::OUTPUT_INTERVAL {
                        self.flush_shell_outputs().await;
                    }
                    self.publish_throttled();
                },
                command = commands.recv() => {
                    let Some(command) = command else {
                        self.publish_throttled();
                        return None;
                    };
                    if !self.running_command(command).await {
                        return None;
                    }
                }
            }
        }
    }

    async fn running_command(&mut self, command: Command) -> bool {
        match command {
            Command::Shared(command) => self.handle_shared(command).await,
            Command::TaskFinished { task_id } => {
                self.queue_task_completion(task_id).await;
            }
            Command::WatchSession {
                session_id,
                receipt,
                background,
                reply,
            } => {
                self.watch_session(session_id, receipt, background);
                let _ = reply.send(Ok(()));
            }
            Command::SessionFinished { report, background } => {
                self.async_count -= 1;
                self.queue_session_completion(report, background).await;
            }
            Command::Note { text, reply } => {
                tracing::debug!("运行中收到提示，入队: {} 字符", text.chars().count());
                self.pending_notes.push(text);
                let _ = reply.send(Ok(()));
            }
            Command::Deliver { delivery, reply } => {
                // 程序化消息不拒绝：按 mode 在安全边界注入或轮次结束后处理
                self.pending_deliveries.push(delivery);
                let _ = reply.send(Ok(()));
            }
            Command::Submit {
                prompt,
                images,
                reply,
                ..
            } => {
                let result = self.queue_input(prompt, images).await;
                let _ = reply.send(result);
            }
            Command::Revert { reply, .. } => {
                let _ = reply.send(Err(BUSY.into()));
            }
            Command::Stop { delete, reply } => {
                self.cancel_pending_user_question();
                self.stop_request = Some((delete, reply));
                self.publish_throttled();
                return false;
            }
        }
        true
    }

    async fn ready_commands(&mut self, commands: &mut mpsc::Receiver<Command>) -> bool {
        // 只处理边界到达时已入通道的命令，持续输入不会阻塞执行。
        for _ in 0..commands.len() {
            if let Ok(command) = commands.try_recv()
                && !self.running_command(command).await
            {
                return false;
            }
        }
        true
    }

    /// 运行中 (drive) 与空闲 (run) 语义一致的命令
    async fn handle_shared(&mut self, command: SharedCommand) {
        match command {
            SharedCommand::SetInputMode { id, mode, reply } => {
                let result = self.set_input_mode(&id, mode);
                let _ = reply.send(result);
            }
            SharedCommand::TakeInput { id, reply } => {
                let result = self.take_input(&id);
                let _ = reply.send(result);
            }
            SharedCommand::Configure {
                model,
                variant,
                reply,
            } => {
                let _ = reply.send(self.configure(model, variant).await);
            }
            SharedCommand::Title(title) => {
                if let Err(error) = self.title(title).await {
                    self.event(AgentEvent::Notice(error)).await;
                }
            }
            SharedCommand::Rename { title, reply } => {
                if let Some(task) = self.title_task.take() {
                    task.abort();
                }
                let _ = reply.send(self.title(title).await);
            }
            SharedCommand::Supersede { by, reply } => {
                let result = self
                    .update_header(|header| header.superseded_by = Some(by))
                    .await;
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
    pub fn dismiss_notice(&mut self, id: &str) {
        self.view
            .notices
            .retain(|notice| notice.id != id || matches!(notice.kind, NoticeKind::Capability));
        self.publish();
    }

    /// 单个用户轮次：反复「模型响应 → 并行工具」直至模型不再调用工具
    pub async fn execute(
        &mut self,
        engine: ExecutionPlan,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<TurnOutcome, String> {
        self.event(AgentEvent::Capabilities(engine.notices.clone()))
            .await;
        self.view.external_web = engine.external_web.clone();
        let definitions = engine.tools.definitions();
        let worked_started = Instant::now();
        loop {
            if !self.ready_commands(commands).await {
                return Ok(TurnOutcome::Aborted);
            }
            self.drain_task_completions().await?;
            self.drain_subagent_completions().await?;
            self.drain_notes().await?;
            self.drain_steering().await?;
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
            log_response(
                &self.view.session.header.id,
                &response.messages,
                response.usage.as_ref(),
                response_started.elapsed(),
            );
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
            turn.worked_ms = (calls.is_empty()
                && !self
                    .view
                    .pending_inputs
                    .iter()
                    .any(|input| input.mode == InputMode::Steer)
                && !self
                    .pending_deliveries
                    .iter()
                    .any(|delivery| delivery.mode == InputMode::Steer))
            .then(|| worked_started.elapsed().as_millis() as u64);
            turn.meta = response.meta;
            self.view.draft.clear();
            self.append(turn).await?;
            if calls.is_empty() {
                if self.drain_steering().await? {
                    continue;
                }
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
                self.setup
                    .abort_children(&self.view.session.header.id)
                    .await;
                return Ok(TurnOutcome::Aborted);
            };
            let batch = self.commit_tool_results(calls, results).await?;
            for message in self.setup.after_tools(&self.view.session, &batch).await? {
                self.append(Turn::user_turn(message)).await?;
            }
        }
    }

    /// 并行执行一批工具调用，结果按调用顺序返回；被 Stop 中断时返回 None
    async fn run_tools(
        &mut self,
        engine: &ExecutionPlan,
        calls: &[crate::model::ToolCall],
        commands: &mut mpsc::Receiver<Command>,
        events: &mut mpsc::Receiver<AgentEvent>,
    ) -> Option<Vec<crate::model::ToolResult>> {
        for call in calls {
            if engine.tools.validate_call(call).is_ok() {
                if call.name == names::SHELL {
                    self.shell_calls.insert(call.id.clone());
                }
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
        let completed = self.drive(collect, commands, events).await;
        // 取消会丢弃 JoinSet；先回收子任务，让 capture 的析构终态进入本次冲刷。
        if completed.is_none() {
            set.abort_all();
            while set.join_next().await.is_some() {}
        }
        self.flush_shell_outputs().await;
        completed?;
        Some(results.into_iter().map(Option::unwrap).collect())
    }

    /// 逐个提交工具结果：附件复验 → ToolFinished 事件 → Tool 轮次落盘，
    /// 返回配对完成的执行记录，交给宿主处理工具后上下文。
    async fn commit_tool_results(
        &mut self,
        calls: Vec<crate::model::ToolCall>,
        results: Vec<crate::model::ToolResult>,
    ) -> Result<Vec<crate::harness::ToolExecution>, String> {
        let mut batch = Vec::new();
        for (call, mut result) in calls.into_iter().zip(results) {
            if call.name == names::SHELL
                && result.background_task_id.is_none()
                && result.shell_output.is_none()
            {
                result.shell_output = self
                    .view
                    .shell_outputs
                    .get(&call.id)
                    .map(|output| Box::new(output.as_ref().clone()));
            }
            // Configure can run while tools are reading; validate before committing their media.
            if !result.attachments.is_empty() {
                let inputs = self.build_engine().await?.inputs;
                if let Some(error) = result
                    .attachments
                    .iter()
                    .find_map(|media| inputs.require(media.kind, &media.mime_type).err())
                {
                    result = crate::model::ToolResult::error(&call.id, error);
                }
            }
            tracing::debug!(
                "工具结果: session={} {} {}{}",
                self.view.session.header.id,
                call.name,
                if result.is_error { "出错 " } else { "" },
                clip_log(&result.content)
            );
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

            batch.push(crate::harness::ToolExecution { call, result });
        }
        Ok(batch)
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
        self.auto_resume = false;
        self.cli = None;
        if delete {
            Arc::make_mut(&mut self.view.pending_inputs).clear();
        }
        self.cancel_deliveries("Session stopped");
        self.turn_tx.send_modify(|completed| {
            if !completed.settled {
                completed.error = Some("Session stopped".into());
                completed.settled = true;
            }
        });
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
    /// 用户中止与供应商错误暂停自动续跑，完成结果仍持久化。
    pub async fn run_turns(
        &mut self,
        mut engine: ExecutionPlan,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<TurnOutcome, String> {
        self.turn_start = self.view.session.turns.len();
        loop {
            let outcome = self.execute(engine, commands).await?;
            if outcome == TurnOutcome::Aborted
                || !self.view.pending_inputs.is_empty()
                || (self.task_completions.is_empty() && self.subagent_completions.is_empty())
            {
                return Ok(outcome);
            }
            engine = self.build_engine().await?;
        }
    }

    /// 轮次收尾：状态复位、终态事件、CLI 通道释放与停止请求处理；
    /// 返回是否终止会话任务
    pub async fn finish_turn(&mut self, result: Result<TurnOutcome, String>) -> bool {
        let mut error = match &result {
            Ok(TurnOutcome::Completed) => None,
            Ok(TurnOutcome::Aborted) => Some("Turn aborted".to_string()),
            Err(error) => Some(error.clone()),
        };
        if let Some(error) = &error {
            self.auto_resume = false;
            self.cancel_deliveries(error);
        }
        let mut text = if error.is_none() {
            self.view.session.turns[self.turn_start..]
                .iter()
                .rev()
                .find(|turn| turn.role == Role::Assistant)
                .map(Turn::final_text)
                .unwrap_or_default()
        } else {
            String::new()
        };
        // 末次模型调用期间到达的提示在轮次收尾处追加（不续跑）
        if let Err(error) = self.drain_notes().await {
            tracing::warn!("Failed to append queued notes: {error}");
        }
        self.view.state = RunState::Idle;
        match result {
            Ok(TurnOutcome::Completed) => self.event(AgentEvent::Finished).await,
            Ok(TurnOutcome::Aborted) => self.event(AgentEvent::Aborted).await,
            Err(error) => self.event(AgentEvent::Error(error)).await,
        }
        // 完成结果在安全轮隙持久化；取消和失败保持原有不自动续跑行为。
        for result in [
            self.drain_task_completions().await,
            self.drain_subagent_completions().await,
        ] {
            if let Err(reason) = result {
                let reason = format!("Failed to persist background result: {reason}");
                self.auto_resume = false;
                self.cancel_deliveries(&reason);
                text.clear();
                error = Some(reason.clone());
                self.event(AgentEvent::Error(reason)).await;
            }
        }
        if !self.auto_resume || self.pending_background() == 0 {
            self.cli = None;
        }
        self.publish();
        // 已完成轮次计数递增，唤醒内部订阅者。
        let seq = self.turn_tx.borrow().seq + 1;
        let settled = self.pending_background() == 0 || !self.auto_resume;
        self.turn_tx.send_replace(CompletedTurn {
            seq,
            text,
            error,
            settled,
        });
        if self.pending_background() == 0 {
            let report = self
                .turn_tx
                .borrow()
                .subagent_report(self.view.session.header.id.clone(), &self.view.session);
            for delivery in std::mem::take(&mut self.active_deliveries) {
                let _ = delivery.completion.send(Ok(report.clone()));
            }
        }
        if let Some((delete, reply)) = self.stop_request.take() {
            return self.stop(delete, reply).await;
        }
        false
    }

    fn cancel_deliveries(&mut self, error: &str) {
        for delivery in std::mem::take(&mut self.pending_deliveries)
            .into_iter()
            .chain(std::mem::take(&mut self.active_deliveries))
        {
            let _ = delivery.completion.send(Err(error.into()));
        }
    }

    /// 同一轮次间隙的投递合并处理，每个投递者获得该轮固定的完成报告。
    pub async fn run_pending_deliveries(&mut self, commands: &mut mpsc::Receiver<Command>) -> bool {
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
            self.active_deliveries.extend(deliveries);
            let result = self.run_turns(engine, commands).await;
            let successful = matches!(result, Ok(TurnOutcome::Completed));
            let close = self.finish_turn(result).await;
            if close || !successful {
                return close;
            }
            if self.run_pending_inputs(commands).await {
                return true;
            }
        }
        false
    }

    /// 空闲时完成通知已入队：自动续跑，execute 首轮注入通知；返回真表示终止会话任务
    async fn run_completion_turn(&mut self, commands: &mut mpsc::Receiver<Command>) -> bool {
        if !self.auto_resume {
            if let Err(error) = self.drain_task_completions().await {
                self.event(AgentEvent::Notice(error)).await;
            }
            if let Err(error) = self.drain_subagent_completions().await {
                self.event(AgentEvent::Notice(error)).await;
            }
            self.publish();
            return false;
        }
        self.view.state = RunState::Thinking;
        self.publish();
        match self.build_engine().await {
            Ok(engine) => {
                let result = self.run_turns(engine, commands).await;
                self.finish_turn(result).await
                    || self.run_pending_inputs(commands).await
                    || self.run_pending_deliveries(commands).await
            }
            // 组装失败同样持久化完成结果，并暂停自动续跑。
            Err(error) => self.finish_turn(Err(error)).await,
        }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            let command = tokio::select! {
                command = commands.recv() => match command { Some(command) => command, None => break },
                _ = self.shell_outputs.notified(), if !self.output_pending => { self.output_pending = true; continue; },
                _ = tokio::time::sleep_until(self.last_output_publish + crate::tools::shell_output::OUTPUT_INTERVAL), if self.output_pending => { self.flush_shell_outputs().await; continue; },
            };
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
                        || self.run_pending_inputs(&mut commands).await
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
                Command::WatchSession {
                    session_id,
                    receipt,
                    background,
                    reply,
                } => {
                    self.watch_session(session_id, receipt, background);
                    let _ = reply.send(Ok(()));
                }
                Command::SessionFinished { report, background } => {
                    self.async_count -= 1;
                    self.queue_session_completion(report, background).await;
                    if self.run_completion_turn(&mut commands).await {
                        break;
                    }
                }
                Command::Note { text, reply } => {
                    let _ = reply.send(self.note(text).await);
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

/// debug 日志中单条内容的字符上限；每条日志只占一行，粒度对齐 web 折叠视图
const LOG_CONTENT_LIMIT: usize = 100;

/// 压成单行（空白折叠）并按字符数截断
fn clip_log(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(LOG_CONTENT_LIMIT) {
        Some((end, _)) => format!("{}…", &flat[..end]),
        None => flat,
    }
}

/// 工具调用的单行目标摘要：取最能代表意图的入参，缺省退回参数原文
fn tool_target(arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    [
        "intent",
        "description",
        "path",
        "pattern",
        "query",
        "url",
        "command",
        "input",
        "prompt",
    ]
    .iter()
    .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
    .map_or_else(|| clip_log(arguments), clip_log)
}

/// 一次模型响应的 debug 细节：推理、文字、工具调用（含参数）与 token 用量
fn log_response(
    session_id: &str,
    messages: &[Message],
    usage: Option<&crate::model::TokenUsage>,
    elapsed: std::time::Duration,
) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    for part in messages.iter().flat_map(|m| &m.parts) {
        match part {
            ContentPart::Reasoning { content, .. } if !content.trim().is_empty() => {
                tracing::debug!("推理: session={session_id} {}", clip_log(content));
            }
            ContentPart::Text { text, .. } if !text.trim().is_empty() => {
                tracing::debug!("文字输出: session={session_id} {}", clip_log(text));
            }
            ContentPart::ToolCall(call) => tracing::debug!(
                "工具调用: session={session_id} {} {}",
                call.name,
                tool_target(&call.arguments)
            ),
            _ => {}
        }
    }
    tracing::debug!(
        "模型响应完成: session={session_id} 耗时 {:.1}s prompt/completion/cached tokens={}/{}/{}",
        elapsed.as_secs_f32(),
        usage.map_or(0, |u| u.prompt_tokens),
        usage.map_or(0, |u| u.completion_tokens),
        usage.and_then(|u| u.cached_tokens).unwrap_or(0)
    );
}

fn shell_call_ids(session: &Session) -> std::collections::HashSet<String> {
    session
        .turns
        .iter()
        .flat_map(|turn| &turn.messages)
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            ContentPart::ToolCall(call) if call.name == names::SHELL => Some(call.id.clone()),
            _ => None,
        })
        .collect()
}
