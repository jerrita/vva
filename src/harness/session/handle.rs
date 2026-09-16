use super::*;

pub enum Command {
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
    AnswerUser {
        request_id: String,
        answers: Vec<UserAnswer>,
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
    pub(super) commands: mpsc::Sender<Command>,
    pub(super) view: watch::Receiver<Arc<SessionView>>,
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

    pub(super) async fn request(
        &self,
        command: impl FnOnce(Reply) -> Command,
    ) -> Result<(), String> {
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

    pub async fn answer_user(
        &self,
        request_id: impl Into<String>,
        answers: Vec<UserAnswer>,
    ) -> Result<(), String> {
        self.request(|reply| Command::AnswerUser {
            request_id: request_id.into(),
            answers,
            reply,
        })
        .await
    }

    pub async fn revert(&self, turn_index: usize) -> Result<(), String> {
        self.request(|reply| Command::Revert { turn_index, reply })
            .await
    }
}
