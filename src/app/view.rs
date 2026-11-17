use super::{model::Session, records::SessionRecord};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
use vva_core::{
    harness::{Notice, PendingInput, RunState, SessionView as CoreView},
    model::{BackgroundTask, BackgroundTaskStatus, ContentPart, ShellOutput, UserQuestionRequest},
};
#[derive(Clone)]
pub struct SessionView {
    pub session: Arc<Session>,
    pub revision: u64,
    pub background_finished: BTreeMap<String, Instant>,
    pub draft: Vec<Arc<ContentPart>>,
    pub notices: Vec<Notice>,
    pub shell_outputs: Arc<BTreeMap<String, Arc<ShellOutput>>>,
    pub background_tasks: Arc<BTreeMap<String, BackgroundTask>>,
    pub external_web: Vec<String>,
    pub state: RunState,
    pub persisted: bool,
    pub pending_user_question: Option<UserQuestionRequest>,
    pub pending_inputs: Arc<Vec<PendingInput>>,
    pub(crate) application_notices: Vec<Notice>,
    pub(crate) core_revision: u64,
    pub(crate) record: Option<SessionRecord>,
}
impl SessionView {
    pub fn new(session: Session, persisted: bool) -> Self {
        let core = CoreView::new(session.to_core(), persisted);
        Self {
            session: Arc::new(session),
            revision: 0,
            background_finished: BTreeMap::new(),
            draft: core.draft,
            notices: core.notices,
            shell_outputs: core.shell_outputs,
            background_tasks: core.background_tasks,
            external_web: core.external_web,
            state: core.state,
            persisted: core.persisted,
            pending_user_question: core.pending_user_question,
            pending_inputs: core.pending_inputs,
            application_notices: Vec::new(),
            core_revision: core.revision,
            record: None,
        }
    }
    pub fn project(core: CoreView, record: &SessionRecord, previous: Option<&Self>) -> Self {
        let mut finished = previous
            .map(|view| view.background_finished.clone())
            .unwrap_or_default();
        for (id, task) in core.background_tasks.iter() {
            if task.status != BackgroundTaskStatus::Running && task.activity.is_some() {
                finished.entry(id.clone()).or_insert_with(Instant::now);
            }
        }
        finished.retain(|id, _| core.background_tasks.contains_key(id));
        let revision = previous.map_or(0, |view| view.revision + 1);
        let session = match previous {
            Some(previous)
                if Arc::ptr_eq(&previous.session.core, &core.session)
                    && previous.record.as_ref() == Some(record) =>
            {
                previous.session.clone()
            }
            _ => Arc::new(Session::from_core(core.session.clone(), record)),
        };
        let application_notices = previous
            .map(|view| view.application_notices.clone())
            .unwrap_or_default();
        let mut notices = core.notices;
        notices.extend(application_notices.iter().cloned());
        Self {
            session,
            revision,
            background_finished: finished,
            draft: core.draft,
            notices,
            shell_outputs: core.shell_outputs,
            background_tasks: core.background_tasks,
            external_web: core.external_web,
            state: core.state,
            persisted: core.persisted,
            pending_user_question: core.pending_user_question,
            pending_inputs: core.pending_inputs,
            application_notices,
            core_revision: core.revision,
            record: Some(record.clone()),
        }
    }
}
pub trait RunStateDisplay {
    fn status(&self) -> String;
}
impl RunStateDisplay for RunState {
    fn status(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Thinking => "Thinking...".into(),
            Self::Responding => "Responding...".into(),
            Self::Tool(name) => format!("Executing tool: {name}"),
        }
    }
}
pub trait BackgroundTaskDisplay {
    fn label(&self) -> String;
}
impl BackgroundTaskDisplay for BackgroundTask {
    fn label(&self) -> String {
        match self.status {
            BackgroundTaskStatus::Running => "后台运行中".into(),
            BackgroundTaskStatus::Completed => "已完成".into(),
            BackgroundTaskStatus::Failed => self
                .exit_code
                .map_or_else(|| "失败".into(), |code| format!("退出码 {code}")),
            BackgroundTaskStatus::Stopped => "已停止".into(),
            BackgroundTaskStatus::Interrupted => "已中断".into(),
        }
    }
}
