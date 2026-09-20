use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::ResponseProvider;
use super::image::project_image_generation_call;
use super::search::project_web_search_call;
use crate::event::AgentEvent;
use crate::model::{Annotation, ContentPart, ResponseOutputItem, WebSearchCall};
use crate::provider::common::{
    EventForwarder, from_value, require_str, require_u64, require_value,
};
use crate::provider::http::SseEventHandler;
use crate::provider::{ProviderError, ProviderResponse};
/// Responses API 流式事件累积处理器
pub(super) struct ResponseStreamHandler<'a> {
    provider: &'a ResponseProvider,
    forwarder: EventForwarder,
    pub(super) completed: Option<ProviderResponse>,
    pub(super) response_id: Option<String>,
    pub(super) last_event_type: Option<String>,
    pub(super) last_sequence_number: Option<u64>,
    pub(super) event_count: u64,
    /// `output_item.done` 投影：领域 WebSearchCall 的条目快照（含当时 sources）
    streamed_searches: HashMap<String, WebSearchCall>,
    warned_missing_search_actions: HashSet<String>,
}

impl<'a> ResponseStreamHandler<'a> {
    pub(super) fn new(
        provider: &'a ResponseProvider,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Self {
        Self {
            provider,
            forwarder: EventForwarder::new(event_sender),
            completed: None,
            response_id: None,
            last_event_type: None,
            last_sequence_number: None,
            event_count: 0,
            streamed_searches: HashMap::new(),
            warned_missing_search_actions: HashSet::new(),
        }
    }

    async fn warn_missing_search_action(&mut self, id: &str) {
        if self.warned_missing_search_actions.insert(id.to_string()) {
            self.forwarder
                .notice(format!(
                    "Responses API web_search_call '{id}' omitted action; displaying an empty search."
                ))
                .await;
        }
    }

    /// 领域 WebSearchCall 用 item.done 快照；native `output_items` 仍是 completed.output。
    fn apply_streamed_searches(&self, parsed: &mut ProviderResponse) {
        for part in parsed
            .messages
            .iter_mut()
            .flat_map(|message| message.parts.iter_mut())
        {
            let ContentPart::WebSearchCall(call) = part else {
                continue;
            };
            if let Some(done) = self.streamed_searches.get(&call.id) {
                call.clone_from(done);
            }
        }
    }
}

#[async_trait]
impl SseEventHandler for ResponseStreamHandler<'_> {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError> {
        let ev_type = require_str(&json, "type")?;
        self.event_count += 1;
        self.last_event_type = Some(ev_type.to_string());
        self.last_sequence_number = json.get("sequence_number").and_then(|value| value.as_u64());
        if let Some(response_id) = json
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(|id| id.as_str())
        {
            self.response_id = Some(response_id.to_string());
        }
        match ev_type {
            "error" => {
                let msg = require_str(&json, "message")?;
                Err(ProviderError::Protocol(format!("Stream error: {msg}")))
            }
            "response.failed" => {
                let resp_obj = require_value(&json, "response")?;
                let err_msg = resp_obj
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Response generation failed");
                Err(ProviderError::Protocol(format!(
                    "Response generation failed: {err_msg}"
                )))
            }
            "response.output_text.delta" => {
                let delta = require_str(&json, "delta")?;
                let text_id = response_text_id(&json)?;
                self.forwarder.text(&text_id, delta).await;
                Ok(())
            }
            "response.output_text.annotation.added" => {
                let text_id = response_text_id(&json)?;
                let annotation = require_value(&json, "annotation")?;
                if require_str(annotation, "type")? == "url_citation" {
                    let annotation: Annotation = from_value(annotation, "URL citation")?;
                    self.forwarder.citation(&text_id, annotation).await;
                }
                Ok(())
            }
            "response.reasoning_summary_part.added" => {
                // 官方规范：summary 分多段时每段以该事件起始。各段语义独立，
                // 以空行（markdown 段落边界）分隔，避免渲染时被软换行合并成一行
                let summary_index = require_u64(&json, "summary_index")?;
                if summary_index > 0 {
                    self.forwarder.thought("\n\n").await;
                }
                Ok(())
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let delta = require_str(&json, "delta")?;
                self.forwarder.thought(delta).await;
                Ok(())
            }
            "response.output_item.done" => {
                let item = require_value(&json, "item")?;
                match require_str(item, "type")? {
                    "web_search_call" => {
                        let item: ResponseOutputItem =
                            from_value(item, "Responses API output item")?;
                        if let ResponseOutputItem::WebSearchCall { id, status, action } = item {
                            let missing_action = action.is_none();
                            let call = project_web_search_call(&id, &status, action.as_ref())?;
                            self.streamed_searches.insert(call.id.clone(), call.clone());
                            self.forwarder.web_search_call(call).await;
                            if missing_action {
                                self.warn_missing_search_action(&id).await;
                            }
                        }
                    }
                    "image_generation_call" => {
                        let item: ResponseOutputItem =
                            from_value(item, "Responses API output item")?;
                        if let ResponseOutputItem::ImageGenerationCall {
                            id,
                            status,
                            result,
                            output_format,
                            revised_prompt,
                            ..
                        } = item
                        {
                            let call = project_image_generation_call(
                                &id,
                                &status,
                                result.as_deref(),
                                output_format.as_deref(),
                                revised_prompt.as_deref(),
                            )?;
                            self.forwarder.image_generation_call(call).await;
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            "response.completed" => {
                let resp_obj = require_value(&json, "response")?;
                let mut parsed = self.provider.parse_response(resp_obj.clone())?;
                for item in parsed
                    .meta
                    .response
                    .iter()
                    .flat_map(|meta| &meta.output_items)
                {
                    if let ResponseOutputItem::WebSearchCall {
                        id, action: None, ..
                    } = item
                    {
                        self.warn_missing_search_action(id).await;
                    }
                }
                self.apply_streamed_searches(&mut parsed);
                self.completed = Some(parsed);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn response_text_id(json: &serde_json::Value) -> Result<String, ProviderError> {
    Ok(format!(
        "{}:{}",
        require_str(json, "item_id")?,
        require_u64(json, "content_index")?
    ))
}
