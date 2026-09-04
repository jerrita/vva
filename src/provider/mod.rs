pub mod anthropic;
pub mod common;
pub mod gemini;
pub mod http;
pub mod response;
pub mod sse;

use std::sync::OnceLock;

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::event::AgentEvent;
use crate::model::{Message, Session, TokenUsage, ToolDefinition, TurnMeta};

/// 全局共享 HTTP 客户端：复用连接池，避免每次会话轮次重建 TLS 连接。
/// 连接超时 30s；读空闲超时 300s（SSE 流按相邻 chunk 间隔计时，不设总时长上限）
static SHARED_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn shared_client() -> &'static reqwest::Client {
    SHARED_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build shared HTTP client")
    })
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
    #[error("Protocol error: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Default)]
pub struct ProviderCapabilities {
    pub supports_native_search: bool,
    pub supports_prompt_caching: bool,
    pub supports_thinking_signature: bool,
    pub supports_stateful_response_id: bool,
    /// 是否消费 CompletionOptions::reasoning_effort（Responses API 的 reasoning.effort 参数）
    pub supports_reasoning_effort: bool,
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

    /// 规范化请求端点 URL（`stream` 标志供以端点区分流式方法的协议使用，如 Gemini streamGenerateContent）
    fn endpoint_url(&self, model: &str, stream: bool) -> Result<String, ProviderError>;

    /// 鉴权与协议 Header
    fn auth_headers(&self) -> Result<HeaderMap, ProviderError>;

    /// 将领域会话转换为该供应商的原生请求体
    fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<serde_json::Value, ProviderError>;

    /// 解析该供应商的原生响应体为统一 ProviderResponse
    fn parse_response(&self, json: serde_json::Value) -> Result<ProviderResponse, ProviderError>;

    /// 非流式补全（如 session 标题生成）：构建 → POST → 解析 的通用模板
    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        let body = self.build_request_body(session, tools, options)?;
        let url = self.endpoint_url(&options.model, false)?;
        let json_resp = http::post_json(shared_client(), &url, self.auth_headers()?, &body).await?;
        self.parse_response(json_resp)
    }

    /// 执行流式会话交互补全，并通过 event_sender 实时发送增量事件 (Thought / Text / Citation)
    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError>;
}
