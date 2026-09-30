//! 会话运行时：句柄表（manager）、RPC 句柄（handle）、执行任务（task）与发布视图（view）。

mod handle;
mod manager;
mod task;
#[cfg(test)]
mod tests;
mod view;

pub use handle::ManagedSession;
use handle::{Command, CompletedTurn, DeliveryReceipt, PendingDelivery};
pub use manager::SessionManager;
use task::{SessionTask, TaskDeps};
pub use view::{Notice, NoticeKind, RunState, SessionView};

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

use super::agent::{AgentEngine, EngineDeps, generate_title};
use crate::config::{AppConfig, AppState};
use crate::event::AgentEvent;
use crate::model::{
    ContentPart, MediaSource, Message, Role, Session, Turn, UserAnswer, UserQuestionRequest,
};
use crate::storage::SessionStore;

/// 运行中拒绝新提示的统一错误文案
const BUSY: &str = "Session is currently running another prompt";

type Reply = oneshot::Sender<Result<(), String>>;

/// 流式增量的视图发布间隔（终端重绘为 100ms，此处取其三分之一）
const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);
