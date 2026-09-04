pub mod anthropic;
pub mod gemini;
pub mod response;
pub mod sse;

use std::sync::OnceLock;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::event::AgentEvent;
use crate::model::{Message, Session, TokenUsage, ToolDefinition, TurnMeta};

/// 全局共享 HTTP 客户端：复用连接池，避免每次会话轮次重建 TLS 连接
static SHARED_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn shared_client() -> &'static reqwest::Client {
    SHARED_CLIENT.get_or_init(reqwest::Client::new)
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error (status {status}): {body}")]
    ApiError {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Protocol error: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Default)]
pub struct ProviderCapabilities {
    pub supports_native_search: bool,
    pub supports_prompt_caching: bool,
    pub supports_thinking_signature: bool,
    pub supports_stateful_response_id: bool,
}

#[derive(Debug, Clone)]
pub struct CompletionOptions {
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub enable_web_search: bool,
    pub reasoning_effort: Option<String>,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            model: String::new(),
            temperature: None,
            max_tokens: Some(4096),
            enable_web_search: true,
            reasoning_effort: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub messages: Vec<Message>,
    pub usage: Option<TokenUsage>,
    pub meta: TurnMeta,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// 供应商名称（如 "response", "anthropic", "gemini"）
    fn name(&self) -> &str;

    /// 供应商原生能力声明
    fn capabilities(&self) -> ProviderCapabilities;

    /// 执行会话交互补全 (非流式，如 session 标题生成)
    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError>;

    /// 执行流式会话交互补全，并通过 event_sender 实时发送增量事件 (Thought / Text / Citation)
    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError>;
}
