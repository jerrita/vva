use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use super::{ResponseProvider, stream::ResponseStreamHandler};
use crate::event::AgentEvent;
use crate::model::{Session, ToolDefinition};
use crate::provider::common::normalize_endpoint;
use crate::provider::http::SseEventHandler;
use crate::provider::websocket::JsonWebSocket;
use crate::provider::{CompletionOptions, ProviderError, ProviderResponse};

type Fingerprint = [u8; 32];

pub(super) struct WebSocketSession {
    connection: JsonWebSocket,
    session_id: String,
    affinity: Option<String>,
    parameters: Fingerprint,
    baseline: Vec<Fingerprint>,
    response_id: String,
}

impl WebSocketSession {
    pub(super) fn is_closed(&self) -> bool {
        self.connection.is_closed()
    }
}

fn terminal(value: &Value) -> bool {
    matches!(
        value["type"].as_str(),
        Some("response.completed" | "response.failed" | "response.incomplete" | "error")
    )
}

fn fingerprint(value: &Value) -> Result<Fingerprint, ProviderError> {
    serde_json::to_vec(value)
        .map(|bytes| Sha256::digest(bytes).into())
        .map_err(|error| ProviderError::Protocol(format!("Cannot encode Responses input: {error}")))
}

impl ResponseProvider {
    pub(super) async fn complete_websocket(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError> {
        let mut body = self.build_full_request_body(session, tools, options)?;
        let mut input = body.as_object_mut().unwrap().remove("input").unwrap();
        let mut baseline = input
            .as_array()
            .unwrap()
            .iter()
            .map(fingerprint)
            .collect::<Result<Vec<_>, _>>()?;
        let parameters = fingerprint(&body)?;

        let mut cached = self.websocket_session.lock().await;
        // 在 await 前取出状态，取消或任何失败都会丢弃连接及其锚点。
        let mut state = match cached.take().filter(|state| !state.is_closed()) {
            Some(state) => state,
            None => WebSocketSession {
                connection: JsonWebSocket::connect(
                    &normalize_endpoint(&self.base_url, "responses"),
                    self.build_auth_headers()?,
                    terminal,
                )
                .await?,
                session_id: String::new(),
                affinity: None,
                parameters,
                baseline: Vec::new(),
                response_id: String::new(),
            },
        };
        let incremental = state.session_id == session.header.id
            && state.affinity == options.affinity
            && state.parameters == parameters
            && !state.response_id.is_empty()
            && baseline.starts_with(&state.baseline);
        if incremental {
            input.as_array_mut().unwrap().drain(..state.baseline.len());
            body["previous_response_id"] = Value::String(state.response_id.clone());
        }
        let input_count = input.as_array().unwrap().len();
        body["input"] = input;
        body["type"] = Value::String("response.create".into());
        let encoded = serde_json::to_string(&body)
            .map_err(|error| ProviderError::Protocol(error.to_string()))?;
        tracing::debug!(
            session = %session.header.id,
            transport = "websocket",
            incremental,
            input_items = input_count,
            request_bytes = encoded.len(),
            "Responses request"
        );
        let mut events = state.connection.request(encoded).await?;
        let mut handler = ResponseStreamHandler::new(self, event_sender);
        loop {
            let event = tokio::time::timeout(Duration::from_secs(300), events.recv())
                .await
                .map_err(|_| {
                    ProviderError::Protocol("Responses WebSocket read timed out".into())
                })?;
            let Some(event) = event else {
                return Err(ProviderError::Protocol(format!(
                    "WebSocket ended without response.completed (response_id={}, last_event={}, event_count={})",
                    handler.response_id.as_deref().unwrap_or("unknown"),
                    handler.last_event_type.as_deref().unwrap_or("none"),
                    handler.event_count,
                )));
            };
            handler.on_event(event?).await?;
            if let Some(mut response) = handler.completed.take() {
                response.stamp_scope(options.affinity.as_deref());
                let output = &response.meta.response.as_ref().unwrap().output_items;
                baseline.extend(
                    output
                        .iter()
                        .map(|item| fingerprint(&serde_json::json!(item)))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                state.session_id = session.header.id.clone();
                state.affinity = options.affinity.clone();
                state.parameters = parameters;
                state.baseline = baseline;
                state.response_id = handler.response_id.take().ok_or_else(|| {
                    ProviderError::Protocol("Missing response ID after response.completed".into())
                })?;
                *cached = Some(state);
                return Ok(response);
            }
        }
    }
}
