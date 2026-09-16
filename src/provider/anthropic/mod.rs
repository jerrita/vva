mod parse;
mod request;
mod stream;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

use crate::event::AgentEvent;
use crate::model::{Session, ToolDefinition};
use crate::provider::common::EventForwarder;
use crate::provider::http::{post_json, post_sse};
use crate::provider::{
    CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse,
};
use parse::{AnthropicUsageSnapshot, assemble_response};
use std::collections::HashMap;
use tokio::sync::mpsc;

use stream::AnthropicStreamHandler;

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com/v1".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 规范化请求端点：base_url 无论是否携带 /messages 后缀，均收敛到唯一端点
    fn normalized_endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/messages").unwrap_or(base);
        format!("{base}/messages")
    }

    fn auth_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_native_search: true,
            supports_reasoning_effort: false,
            server_tool_names: vec!["web_search", "web_fetch"],
        }
    }

    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        let body = self.build_request_body(session, tools, options)?;
        let response = post_json(&self.normalized_endpoint(), self.auth_headers()?, &body).await?;
        self.parse_response(response)
    }

    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError> {
        let mut body = self.build_request_body(session, tools, options)?;
        body["stream"] = serde_json::json!(true);

        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            partial_inputs: HashMap::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(event_sender),
        };
        let _ = post_sse(
            &self.normalized_endpoint(),
            self.auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;
        let (blocks, usage) = handler.finish()?;

        assemble_response(blocks, usage)
    }
}
