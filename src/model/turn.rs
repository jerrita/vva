use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::message::{Message, Role};
use super::meta::TurnMeta;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Turn {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<String>,
    pub role: Role,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default)]
    pub meta: TurnMeta,
}

impl Turn {
    pub fn new(role: Role, messages: Vec<Message>, parent_turn_id: Option<String>) -> Self {
        Self {
            id: format!("turn_{}", Uuid::new_v4().simple()),
            parent_turn_id,
            role,
            messages,
            usage: None,
            meta: TurnMeta::default(),
        }
    }

    pub fn user_turn(message: Message, parent_turn_id: Option<String>) -> Self {
        Self::new(Role::User, vec![message], parent_turn_id)
    }

    pub fn assistant_turn(messages: Vec<Message>, parent_turn_id: Option<String>) -> Self {
        Self::new(Role::Assistant, messages, parent_turn_id)
    }

    /// 获取 OpenAI Responses 链式调用 ID（如果存在）
    pub fn openai_response_id(&self) -> Option<&str> {
        self.meta.response.as_ref().map(|r| r.response_id.as_str())
    }
}

