use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::content::ContentPart;
use super::meta::MessageMeta;
use super::tool::ToolResult;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub parts: Vec<ContentPart>,
    #[serde(default)]
    pub meta: MessageMeta,
}

impl Message {
    pub fn new(role: Role, parts: Vec<ContentPart>) -> Self {
        Self {
            id: format!("msg_{}", Uuid::new_v4().simple()),
            role,
            parts,
            meta: MessageMeta::default(),
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentPart::text(text)])
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, vec![ContentPart::text(text)])
    }

    pub fn assistant(parts: Vec<ContentPart>) -> Self {
        Self::new(Role::Assistant, parts)
    }

    pub fn tool_result(tool_result: ToolResult) -> Self {
        Self::new(Role::Tool, vec![ContentPart::ToolResult(tool_result)])
    }

    pub fn text_content(&self) -> String {
        let mut result = String::new();
        for part in &self.parts {
            if let ContentPart::Text { text, .. } = part {
                result.push_str(text);
            }
        }
        result
    }
}
