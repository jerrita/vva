//! 会话运行时：句柄表（manager）、RPC 句柄（handle）、执行任务（task）与发布视图（view）。

pub(crate) mod handle;
mod input;
mod manager;
pub(crate) mod task;
#[cfg(all(
    test,
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
mod tests;
mod view;

use handle::{Command, PendingDelivery, SharedCommand};
pub use handle::{CompletedTurn, DeliveryReceipt, ManagedSession};
pub use manager::SessionManager;
use task::{SessionTask, TaskDeps};
pub use view::{InputMode, Notice, NoticeKind, PendingInput, RunState, SessionView};

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

use super::ExecutionPlan;
use crate::event::AgentEvent;
use crate::model::{ContentPart, Message, Role, Session, Turn, UserAnswer, UserQuestionRequest};
use crate::storage::SessionStore;

/// 运行中拒绝新提示的统一错误文案
pub const BUSY: &str = "Session is currently running another prompt";

type Reply = oneshot::Sender<Result<(), String>>;

/// 流式增量的视图发布间隔（终端重绘为 100ms，此处取其三分之一）
const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);

#[cfg(all(
    test,
    feature = "response",
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
mod runtime_tests;

#[cfg(all(
    test,
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
mod receipt_tests;

#[cfg(all(
    test,
    feature = "jsonl",
    feature = "shell-tools",
    feature = "filesystem-tools"
))]
mod lifecycle_tests;
