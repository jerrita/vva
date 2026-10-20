mod parse;
mod request;
mod stream;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::common::header_value;
use super::http::{post_json, post_sse};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};
use crate::event::AgentEvent;
use crate::model::{Session, ToolDefinition};
use parse::GeminiAccumulator;

pub struct GeminiProvider {
    api_key: String,
    base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 规范化请求端点：根据 base_url 与模型名拼接 generateContent / streamGenerateContent，
    /// 流式端点附带 alt=sse，API key 以查询参数与 Header 双通道传递
    fn endpoint_url(&self, model: &str, stream: bool) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        let model_name = model.strip_prefix("models/").unwrap_or(model);
        let method = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let path = if base.ends_with("/models") {
            format!("{base}/{model_name}:{method}")
        } else {
            format!("{base}/models/{model_name}:{method}")
        };
        let sep = if path.contains('?') { '&' } else { '?' };
        if stream {
            format!("{path}{sep}alt=sse&key={}", self.api_key)
        } else {
            format!("{path}{sep}key={}", self.api_key)
        }
    }

    /// JSON Content-Type 与 x-goog-api-key 鉴权头
    fn auth_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("x-goog-api-key", header_value(&self.api_key)?);
        Ok(headers)
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_native_search: true,
            supports_reasoning_effort: false,
            // google_search 由服务端以 groundingMetadata 投影，模型不按名调用；
            // url_context 由服务端读取 URL，同样不产生客户端调用，仅声明 web_fetch 在位
            server_tool_names: vec!["web_fetch"],
        }
    }

    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        let body = self.build_request_body(session, tools, options)?;
        let response = post_json(
            &self.endpoint_url(&options.model, false),
            self.auth_headers()?,
            &body,
        )
        .await?;
        GeminiAccumulator::parse_response(&response, tools)
    }

    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError> {
        let body = self.build_request_body(session, tools, options)?;

        let mut handler = GeminiAccumulator::new(event_sender, tools);
        let _ = post_sse(
            &self.endpoint_url(&options.model, true),
            self.auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;
        Ok(handler.finish())
    }
}
