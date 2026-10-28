pub mod anthropic;
pub mod common;
pub mod gemini;
pub mod http;
pub mod response;
pub mod sse;

use std::sync::OnceLock;

use async_trait::async_trait;
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
    /// 是否将 CompletionOptions::reasoning_effort 转换为供应商原生 effort 参数。
    pub supports_reasoning_effort: bool,
    /// 原生服务端工具名（如 web_search / web_fetch）：由 API 端点代执行，
    /// 客户端注册表不可执行；用于模型误发起客户端调用时的显式诊断
    pub server_tool_names: Vec<&'static str>,
}

/// 结构化输出约束：供应商以原生机制强制模型返回符合 `schema` 的 JSON 文本，
/// 由调用方自行反序列化；适配器不修复、不校验返回内容
#[derive(Debug, Clone)]
pub struct ResponseSchema {
    pub name: String,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct CompletionOptions {
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub enable_web_search: bool,
    /// 是否向 API 声明 web_fetch 服务端工具（模型级 `annotation.web_fetch: false` 时关闭）
    pub enable_web_fetch: bool,
    pub reasoning_effort: Option<String>,
    /// 显式关闭思考，优先于 reasoning_effort；适配器转换为供应商原生参数。
    pub disable_thinking: bool,
    /// Anthropic 服务端 web 工具版本后缀（模型级 `annotation.web_tool_version`；原生与独立搜索共用默认值）
    pub web_tool_version: Option<String>,
    /// 请求服务档位（Responses API 的 `service_tier` 参数；模型级 `annotation.fast: true` 时为 `fast`）
    pub service_tier: Option<String>,
    /// 模型亲和组：组装轮次元数据时打标，回放时据此判定原生状态是否可复用
    pub affinity: Option<String>,
    /// 期望的 JSON Schema 输出（Responses `text.format` / Anthropic `output_config.format` / Gemini `responseJsonSchema`）
    pub response_schema: Option<ResponseSchema>,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            model: String::new(),
            temperature: None,
            max_tokens: None,
            enable_web_search: true,
            enable_web_fetch: true,
            reasoning_effort: None,
            disable_thinking: false,
            web_tool_version: None,
            service_tier: None,
            affinity: None,
            response_schema: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub messages: Vec<Message>,
    pub usage: Option<TokenUsage>,
    pub meta: TurnMeta,
}

impl ProviderResponse {
    /// 将当前亲和组写入轮次元数据：回放时据此判定原生状态可否复用
    fn stamp_scope(&mut self, scope: Option<&str>) {
        let scope = scope.map(str::to_string);
        if let Some(meta) = self.meta.response.as_mut() {
            meta.scope = scope;
        } else if let Some(meta) = self.meta.anthropic.as_mut() {
            meta.scope = scope;
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// 供应商名称（如 "response", "anthropic", "gemini"）
    fn name(&self) -> &str;

    /// 供应商原生能力声明
    fn capabilities(&self) -> ProviderCapabilities;

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
