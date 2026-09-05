use serde::{Deserialize, Serialize};

use super::content::ContentPart;
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
    pub parts: Vec<ContentPart>,
}

impl Message {
    pub fn new(parts: Vec<ContentPart>) -> Self {
        Self { parts }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![ContentPart::text(text)])
    }

    pub fn tool_result(tool_result: ToolResult) -> Self {
        Self::new(vec![ContentPart::ToolResult(tool_result)])
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
