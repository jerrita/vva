use super::*;

#[derive(Clone, Default)]
pub(super) struct CompletedTurn {
    pub seq: u64,
    pub text: String,
    pub error: Option<String>,
}

impl CompletedTurn {
    pub(super) fn subagent_report(
        &self,
        session_id: String,
        session: &Session,
    ) -> crate::tools::agent::SubagentReport {
        crate::tools::agent::SubagentReport {
            session_id,
            agent: session.header.agent.clone().unwrap_or_default(),
            title: session.header.title.clone().unwrap_or_default(),
            turn_seq: self.seq,
            text: self.text.clone(),
            error: self.error.clone(),
        }
    }
}

pub(super) type DeliveryResult = Result<crate::tools::agent::SubagentReport, String>;
pub(super) type DeliveryReceipt = oneshot::Receiver<DeliveryResult>;

pub(super) struct PendingDelivery {
    pub text: String,
    pub completion: oneshot::Sender<DeliveryResult>,
}

pub enum SharedCommand {
    Configure {
        model: Option<String>,
        variant: Option<Option<String>>,
        toggles: ToggleOverrides,
        reply: Reply,
    },
    AnswerUser {
        request_id: String,
        answers: Vec<UserAnswer>,
        reply: Reply,
    },
    Title(String),
    /// 用户改名：先取消尚未回写的自动标题任务，避免被覆盖
    Rename {
        title: String,
        reply: Reply,
    },
    /// 标记会话已被 rollover 接替（写入 header.superseded_by）
    Supersede {
        by: String,
        reply: Reply,
    },
    /// 通过稳定身份关闭会话提示
    DismissNotice {
        id: String,
        reply: Reply,
    },
}

pub enum Command {
    Submit {
        prompt: String,
        /// 用户随提示词提交的图片附件（Base64）
        images: Vec<ContentPart>,
        events: Option<mpsc::Sender<AgentEvent>>,
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
    /// 运行中与空闲时语义一致的命令
    Shared(SharedCommand),
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
    /// 只追加提示：空闲直接落盘为 User reminder 轮次，运行中排队到轮次间隙；
    /// 永不触发模型轮次
    Note {
        text: String,
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
        completed.subagent_report(self.id.clone(), &view.session)
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

    /// 程序化投递并等待该轮结束，返回本轮报告；轮次失败或中止时返回错误
    pub async fn deliver_wait(
        &self,
        text: &str,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        let report = self
            .deliver(text)
            .await?
            .await
            .map_err(|_| "Session task stopped".to_string())??;
        match report.error {
            Some(error) => Err(error),
            None => Ok(report),
        }
    }

    /// 追加一条提示（reminder 文本）到历史末尾，不触发模型轮次；
    /// 运行中入队，在下一个轮次间隙（或本轮收尾）追加，不打断当前轮次
    pub async fn note(&self, text: &str) -> Result<(), String> {
        tracing::debug!(
            "追加提示: session={} {} 字符",
            self.id,
            text.chars().count()
        );
        self.request(|reply| Command::Note {
            text: text.into(),
            reply,
        })
        .await
    }

    /// 用户手动改名（覆盖自动生成的标题）
    pub async fn rename(&self, title: &str) -> Result<(), String> {
        self.request(|reply| {
            Command::Shared(SharedCommand::Rename {
                title: title.into(),
                reply,
            })
        })
        .await
    }

    /// 标记本会话已被 `by` 接替
    pub async fn mark_superseded(&self, by: &str) -> Result<(), String> {
        self.request(|reply| {
            Command::Shared(SharedCommand::Supersede {
                by: by.into(),
                reply,
            })
        })
        .await
    }

    pub async fn configure(
        &self,
        model: Option<String>,
        variant: Option<Option<String>>,
        toggles: ToggleOverrides,
    ) -> Result<(), String> {
        self.request(|reply| {
            Command::Shared(SharedCommand::Configure {
                model,
                variant,
                toggles,
                reply,
            })
        })
        .await
    }

    pub async fn abort_current(&self) -> Result<(), String> {
        self.stop(false).await
    }

    /// 停止会话任务并删除其存储记录
    pub(super) async fn delete(&self) -> Result<(), String> {
        self.stop(true).await
    }

    async fn stop(&self, delete: bool) -> Result<(), String> {
        self.request(|reply| Command::Stop { delete, reply }).await
    }

    /// 关闭一条运行通知横幅
    pub async fn dismiss_notice(&self, id: String) -> Result<(), String> {
        self.request(|reply| Command::Shared(SharedCommand::DismissNotice { id, reply }))
            .await
    }

    pub async fn answer_user(
        &self,
        request_id: impl Into<String>,
        answers: Vec<UserAnswer>,
    ) -> Result<(), String> {
        self.request(|reply| {
            Command::Shared(SharedCommand::AnswerUser {
                request_id: request_id.into(),
                answers,
                reply,
            })
        })
        .await
    }

    pub async fn revert(&self, turn_index: usize) -> Result<(), String> {
        self.request(|reply| Command::Revert { turn_index, reply })
            .await
    }
}
