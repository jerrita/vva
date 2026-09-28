use super::*;

#[derive(Clone, Default)]
pub(super) struct CompletedTurn {
    pub seq: u64,
    pub text: String,
    pub error: Option<String>,
}

pub(super) type DeliveryResult = Result<crate::tools::agent::SubagentReport, String>;
pub(super) type DeliveryReceipt = oneshot::Receiver<DeliveryResult>;

pub(super) struct PendingDelivery {
    pub text: String,
    pub completion: oneshot::Sender<DeliveryResult>,
}

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
    /// 关闭一条运行通知横幅（按快照下标，发布间仅追加故下标稳定）
    DismissNotice {
        index: usize,
        reply: Reply,
    },
    /// 后台任务完成：注入通知轮次并自动续跑（无应答，monitor 经 WeakSender 投递）
    TaskFinished {
        task_id: String,
    },
    /// 程序化消息投递（SendMessage / IPC）：运行中入队轮次间隙处理，空闲即运行轮次；
    /// 与用户 Submit 区分，忙时不拒绝
    Deliver {
        delivery: PendingDelivery,
        reply: Reply,
    },
    /// 后台子代理完成：注入最终消息轮次并自动续跑（无应答，watcher 经句柄投递）
    SubagentFinished {
        session_id: String,
    },
}

pub struct ManagedSession {
    pub id: String,
    pub(super) commands: mpsc::Sender<Command>,
    pub(super) view: watch::Receiver<Arc<SessionView>>,
    /// 已完成轮次计数（每次 finish_turn 递增），供外部等待轮次结束
    pub(super) turns: watch::Receiver<CompletedTurn>,
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

    pub fn turn_seq(&self) -> u64 {
        self.turns.borrow().seq
    }

    pub(super) fn turn_receiver(&self) -> watch::Receiver<CompletedTurn> {
        self.turns.clone()
    }

    pub(super) fn report(&self) -> crate::tools::agent::SubagentReport {
        let view = self.snapshot();
        let completed = self.turns.borrow();
        crate::tools::agent::SubagentReport {
            session_id: self.id.clone(),
            agent: view.session.agent.clone().unwrap_or_default(),
            title: view.session.title.clone().unwrap_or_default(),
            turn_seq: completed.seq,
            text: completed.text.clone(),
            error: completed.error.clone(),
        }
    }

    /// 后台子代理完成回投（watcher 调用）：通道饱和或会话退出时丢弃
    pub(super) fn notify_subagent_finished(&self, session_id: String) {
        let _ = self
            .commands
            .try_send(Command::SubagentFinished { session_id });
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

    /// 程序化消息投递：运行中排队，空闲即触发轮次
    pub(super) async fn deliver(&self, text: &str) -> Result<DeliveryReceipt, String> {
        let (completion, receipt) = oneshot::channel();
        self.request(|reply| Command::Deliver {
            delivery: PendingDelivery {
                text: text.into(),
                completion,
            },
            reply,
        })
        .await?;
        Ok(receipt)
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

    /// 关闭一条运行通知横幅
    pub async fn dismiss_notice(&self, index: usize) -> Result<(), String> {
        self.request(|reply| Command::DismissNotice { index, reply })
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
