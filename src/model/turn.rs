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
    pub role: Role,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// 本次模型响应耗时，单位毫秒。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// 当前用户请求从开始到最终响应完成的总耗时，单位毫秒。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worked_ms: Option<u64>,
    #[serde(default)]
    pub meta: TurnMeta,
}

impl Turn {
    pub fn new(role: Role, messages: Vec<Message>) -> Self {
        Self {
            id: format!("turn_{}", Uuid::new_v4().simple()),
            role,
            messages,
            usage: None,
            elapsed_ms: None,
            worked_ms: None,
            meta: TurnMeta::default(),
        }
    }

    pub fn user_turn(message: Message) -> Self {
        Self::new(Role::User, vec![message])
    }

    pub fn assistant_turn(messages: Vec<Message>) -> Self {
        Self::new(Role::Assistant, messages)
    }

    pub fn text_content(&self) -> String {
        self.messages
            .iter()
            .map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 获取 OpenAI Responses 链式调用 ID（仅在服务端启用 store 时有效）
    pub fn openai_response_id(&self) -> Option<&str> {
        let meta = self.meta.response.as_ref()?;
        if !meta.store {
            return None;
        }
        meta.response_id.as_deref()
    }
}
