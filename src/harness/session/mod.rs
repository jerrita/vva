//! 会话运行时：句柄表（manager）、RPC 句柄（handle）、执行任务（task）与发布视图（view）。

mod handle;
mod manager;
mod task;
#[cfg(test)]
mod tests;
mod view;

pub(super) use handle::Command;
pub use handle::ManagedSession;
pub use manager::SessionManager;
use task::SessionTask;
pub use view::{RunState, SessionView};

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

use super::agent::{AgentEngine, capability_notices, generate_title};
use crate::config::{AppConfig, AppState};
use crate::event::AgentEvent;
use crate::model::{
    ContentPart, MediaSource, Message, Role, Session, Turn, UserAnswer, UserQuestionRequest,
};
use crate::storage::SessionStore;

type Reply = oneshot::Sender<Result<(), String>>;

/// 流式增量的视图发布间隔（终端重绘为 100ms，此处取其三分之一）
const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);
