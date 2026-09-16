use super::ResponseProvider;
use super::search::project_web_search_call;
use crate::provider::common::{
    require_array, require_bool, require_str, require_u64, require_value,
};

use crate::model::{
    ContentPart, Message, ResponseMessageContent, ResponseOutputItem, ResponseReasoningContent,
    ResponseReasoningSummary, ResponseTurnMeta, TokenUsage, ToolCall, TurnMeta,
};
use crate::provider::{ProviderError, ProviderResponse};

impl ResponseProvider {
    /// 解析 Responses API 响应体：先反序列化为强类型 output items，
    /// 再投影为领域 ContentPart（解析失败即报协议错误，不做字段兜底）
    pub(super) fn parse_response(
        &self,
        json: serde_json::Value,
    ) -> Result<ProviderResponse, ProviderError> {
        let is_stored = match json.get("store") {
            Some(_) => require_bool(&json, "store")?,
            None => false,
        };

        let raw_id = require_str(&json, "id")?;

        let response_id = if is_stored {
            Some(raw_id.to_string())
        } else {
            None
        };

        let status = require_str(&json, "status")?.to_string();

        if status == "failed" {
            let err_msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Response status is failed");
            return Err(ProviderError::Protocol(format!(
                "Response failed: {err_msg}"
            )));
        }

        let previous_response_id = if is_stored {
            json["previous_response_id"].as_str().map(|s| s.to_string())
        } else {
            None
        };
        let service_tier = json["service_tier"].as_str().map(|s| s.to_string());
        let reasoning_effort = json["reasoning"]["effort"].as_str().map(|s| s.to_string());

        let output_items = require_array(&json, "output")?;

        let mut native_output_items = Vec::with_capacity(output_items.len());
        for item in output_items {
            let native_item: ResponseOutputItem =
                serde_json::from_value(item.clone()).map_err(|e| {
                    ProviderError::Protocol(format!("Invalid Responses API output item: {e}"))
                })?;
            native_output_items.push(native_item);
        }

        let mut parts = Vec::new();

        for item in &native_output_items {
            match item {
                ResponseOutputItem::WebSearchCall { id, status, action } => {
                    parts.push(ContentPart::WebSearchCall(project_web_search_call(
                        id,
                        status,
                        action.as_ref(),
                    )?));
                }
                ResponseOutputItem::Reasoning {
                    summary, content, ..
                } => {
                    let mut texts = Vec::new();
                    for s in summary {
                        let ResponseReasoningSummary::SummaryText { text } = s;
                        texts.push(text.clone());
                    }
                    for c in content {
                        let ResponseReasoningContent::ReasoningText { text } = c;
                        texts.push(text.clone());
                    }
                    // 各 summary/content 段以空行分隔（markdown 段落边界），渲染时独立成行
                    let reasoning_text = texts.join("\n\n");
                    if !reasoning_text.is_empty() {
                        parts.push(ContentPart::Reasoning {
                            content: reasoning_text,
                            signature: None,
                        });
                    }
                }
                ResponseOutputItem::Message { content, .. } => {
                    for c in content {
                        match c {
                            ResponseMessageContent::OutputText { text, annotations } => {
                                parts.push(ContentPart::Text {
                                    text: text.clone(),
                                    annotations: annotations.clone(),
                                });
                            }
                            ResponseMessageContent::Refusal { refusal } => {
                                parts.push(ContentPart::text(refusal.clone()));
                            }
                        }
                    }
                }
                ResponseOutputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    parts.push(ContentPart::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    }));
                }
            }
        }

        let mut usage = None;
        if let Some(u) = json.get("usage")
            && !u.is_null()
        {
            let input_tokens = require_u64(u, "input_tokens")? as u32;
            let output_tokens = require_u64(u, "output_tokens")? as u32;
            let total_tokens = require_u64(u, "total_tokens")? as u32;

            let input_details = require_value(u, "input_tokens_details")?;
            let cached = require_u64(input_details, "cached_tokens")? as u32;

            let output_details = require_value(u, "output_tokens_details")?;
            let reasoning = output_details["reasoning_tokens"].as_u64().ok_or_else(|| {
                ProviderError::Protocol(
                    "Missing 'reasoning_tokens' in output_tokens_details".into(),
                )
            })? as u32;

            usage = Some(TokenUsage {
                prompt_tokens: input_tokens,
                completion_tokens: output_tokens,
                total_tokens,
                cached_tokens: Some(cached),
                reasoning_tokens: Some(reasoning),
            });
        }

        let meta = TurnMeta {
            response: Some(ResponseTurnMeta {
                response_id,
                previous_response_id,
                status,
                service_tier,
                reasoning_effort,
                store: is_stored,
                output_items: native_output_items,
            }),
            ..Default::default()
        };

        let message = Message::new(parts);

        Ok(ProviderResponse {
            messages: vec![message],
            usage,
            meta,
        })
    }
}
