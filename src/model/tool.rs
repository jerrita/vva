use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub tool_call_id: String,
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
            content: content.into(),
            is_error: false,
            attachments: Vec::new(),
        }
    }

    pub fn error(tool_call_id: impl Into<String>, error_message: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
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
    pub parameters: serde_json::Value,
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
