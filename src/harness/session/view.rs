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
    pub notices: Vec<Notice>,
    /// 外置 web 工具接管的后端名（原生能力缺位时），供状态栏标注
    pub external_web: Vec<String>,
    pub state: RunState,
    pub persisted: bool,
    pub pending_user_question: Option<UserQuestionRequest>,
}

impl SessionView {
    pub(super) fn new(session: Session, persisted: bool) -> Self {
        Self {
            revision: 0,
            session: Arc::new(session),
            draft: Vec::new(),
            notices: Vec::new(),
            external_web: Vec::new(),
            state: RunState::Idle,
            persisted,
            pending_user_question: None,
        }
    }
}

/// 会话拥有的反馈，身份独立于文本和快照中的位置。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Notice {
    pub id: String,
    pub text: String,
    #[serde(flatten)]
    pub kind: NoticeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoticeKind {
    Session,
    Capability,
    Turn { turn_id: String, aborted: bool },
    Task { task_id: String },
}

impl Notice {
    pub fn new(text: String, kind: NoticeKind) -> Self {
        Self {
            id: format!("notice_{}", uuid::Uuid::new_v4().simple()),
            text,
            kind,
        }
    }

    pub fn is_banner(&self) -> bool {
        matches!(self.kind, NoticeKind::Session | NoticeKind::Capability)
    }
}
