mod image;
mod parse;
mod request;
mod search;
mod stream;
#[cfg(test)]
mod tests;

use async_trait::async_trait;

use super::common::normalize_endpoint;
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};
use crate::event::AgentEvent;
use crate::model::{Session, ToolDefinition};
use crate::provider::http::{post_json, post_sse};
use stream::ResponseStreamHandler;
use tokio::sync::mpsc;

/// OpenAI Responses API 适配器：previous_response_id 链式继承 + 原生搜索。
pub struct ResponseProvider {
    api_key: String,
    base_url: String,
}

impl ResponseProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl Provider for ResponseProvider {
    fn name(&self) -> &str {
        "response"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_native_search: true,
            supports_reasoning_effort: true,
            server_tool_names: vec!["web_search"],
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
            &normalize_endpoint(&self.base_url, "responses"),
            self.build_auth_headers()?,
            &body,
        )
        .await?;
        let mut parsed = self.parse_response(response)?;
        parsed.stamp_scope(options.affinity.as_deref());
        Ok(parsed)
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

        let mut handler = ResponseStreamHandler::new(self, event_sender);
        let termination = post_sse(
            &normalize_endpoint(&self.base_url, "responses"),
            self.build_auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;

        if let Some(completed) = handler.completed.as_mut() {
            completed.stamp_scope(options.affinity.as_deref());
        }
        handler.completed.ok_or_else(|| {
            let response_id = handler.response_id.as_deref().unwrap_or("unknown");
            let last_event = handler.last_event_type.as_deref().unwrap_or("none");
            let sequence = handler
                .last_sequence_number
                .map_or_else(|| "unknown".into(), |value| value.to_string());
            ProviderError::Protocol(format!(
                "Stream ended without response.completed (termination={termination}, response_id={response_id}, last_event={last_event}, sequence_number={sequence}, event_count={})",
                handler.event_count
            ))
        })
    }
}
