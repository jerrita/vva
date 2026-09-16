use super::*;

#[derive(Debug, Clone, Default)]
pub enum RunState {
    #[default]
    Idle,
    Thinking,
    Responding,
    Tool(String),
}

impl RunState {
    pub fn status(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Thinking => "Thinking...".into(),
            Self::Responding => "Responding...".into(),
            Self::Tool(name) => format!("Executing tool: {name}"),
        }
    }
}

/// 会话任务发布的只读视图；草稿与已完成消息使用相同的内容类型。
/// `revision` 每次发布递增，供订阅方判定缓存是否失效。
///
/// 草稿部件以 `Arc` 共享：发布只复制指针序列，修改单个部件仅克隆该部件。
/// 部件身份（`Arc::ptr_eq`）即内容未变判定，订阅方（Web 增量、TUI 渲染缓存）
/// 以身份对齐前缀，避免逐内容比较。文本增量只做追加，保证同身份部件的内容
/// 前缀稳定。
#[derive(Clone)]
pub struct SessionView {
    pub revision: u64,
    pub session: Arc<Session>,
    pub draft: Vec<Arc<ContentPart>>,
    pub notices: Vec<String>,
    pub state: RunState,
    pub persisted: bool,
    pub pending_user_question: Option<UserQuestionRequest>,
}
