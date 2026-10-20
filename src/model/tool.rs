use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Function 调用为 JSON 文本；Custom 调用为工具原始输入文本
    pub arguments: String,
    #[serde(default, skip_serializing_if = "ToolCallKind::is_function")]
    pub kind: ToolCallKind,
}

/// 调用形态：与工具定义的 ToolInput 一一对应
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    #[default]
    Function,
    Custom,
}

impl ToolCallKind {
    pub fn is_function(&self) -> bool {
        matches!(self, Self::Function)
    }
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
            kind: ToolCallKind::Function,
        }
    }

    /// Freeform 工具调用：arguments 为原始输入文本
    pub fn custom(
        id: impl Into<String>,
        name: impl Into<String>,
        input: impl Into<String>,
    ) -> Self {
        Self {
            kind: ToolCallKind::Custom,
            ..Self::new(id, name, input)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub tool_call_id: String,
    /// 后台执行的任务或子会话身份，用于关联运行状态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_task_id: Option<String>,
    /// 启动或查询时的结构化快照；原始工具输出保持不变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<super::BackgroundTask>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MediaContent>,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            background_task_id: None,
            task: None,
            content: content.into(),
            is_error: false,
            attachments: Vec::new(),
        }
    }

    pub fn error(tool_call_id: impl Into<String>, error_message: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            background_task_id: None,
            task: None,
            content: error_message.into(),
            is_error: true,
            attachments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input: ToolInput,
}

/// 工具入参形态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolInput {
    /// JSON Schema 描述的对象参数
    Json(serde_json::Value),
    /// 原始文本输入；grammar 为可选的 Lark 文法约束
    Freeform { grammar: Option<String> },
}

#[cfg(test)]
impl ToolInput {
    pub fn expect_json(self) -> serde_json::Value {
        match self {
            Self::Json(schema) => schema,
            Self::Freeform { .. } => panic!("expected JSON tool input"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserQuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserQuestion {
    pub header: String,
    pub question: String,
    pub options: Vec<UserQuestionOption>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserQuestionRequest {
    pub id: String,
    pub questions: Vec<UserQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserAnswer {
    pub question: String,
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Pdf,
    Audio,
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaContent {
    pub kind: MediaKind,
    pub mime_type: String,
    pub filename: String,
    pub data: String,
}
