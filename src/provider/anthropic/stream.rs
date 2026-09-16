use std::collections::HashMap;

use async_trait::async_trait;

use super::parse::{
    AnthropicUsageSnapshot, project_annotation, project_fetch_call, project_search_call,
};
use crate::model::{
    AnthropicCitation, AnthropicContentBlock, AnthropicFetchResultContent,
    AnthropicSearchResultContent,
};
use crate::provider::ProviderError;
use crate::provider::common::{
    EventForwarder, from_value, require_str, require_u64, require_value,
};
use crate::provider::http::SseEventHandler;
pub(super) struct AnthropicStreamHandler {
    pub(super) blocks: Vec<AnthropicContentBlock>,
    /// tool_use / server_tool_use 的增量输入 JSON：官方规范以 input_json_delta 流式下发，
    /// 完整 JSON 由 finish 统一解析回填；server tool 结果块到达时提前解析配对
    pub(super) partial_inputs: HashMap<usize, String>,
    pub(super) usage: AnthropicUsageSnapshot,
    pub(super) forwarder: EventForwarder,
}

impl AnthropicStreamHandler {
    /// 保证 blocks 至少有 `len` 个槽位，不足处以空 Text 块占位
    fn ensure_block_slots(&mut self, len: usize) {
        while self.blocks.len() < len {
            self.blocks.push(AnthropicContentBlock::Text {
                text: String::new(),
                citations: Vec::new(),
            });
        }
    }

    /// 解析指定服务端调用的完整 input（结果块到达时输入流已结束），并回填至原生块
    fn server_use_input(&mut self, tool_use_id: &str) -> Result<serde_json::Value, ProviderError> {
        let idx = self
            .blocks
            .iter()
            .position(
                |b| matches!(b, AnthropicContentBlock::ServerToolUse { id, .. } if id == tool_use_id),
            )
            .ok_or_else(|| {
                ProviderError::Protocol(format!(
                    "server tool result without preceding server_tool_use '{tool_use_id}'"
                ))
            })?;
        let partial = self.partial_inputs.remove(&idx).unwrap_or_default();
        let input: serde_json::Value = if partial.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&partial).map_err(|e| {
                ProviderError::Protocol(format!("Invalid server tool input JSON: {e}"))
            })?
        };
        if let AnthropicContentBlock::ServerToolUse {
            input: block_input, ..
        } = &mut self.blocks[idx]
        {
            *block_input = input.clone();
        }
        Ok(input)
    }

    /// 流结束：将累积的增量输入解析回填，产出原生块链
    pub(super) fn finish(
        mut self,
    ) -> Result<(Vec<AnthropicContentBlock>, AnthropicUsageSnapshot), ProviderError> {
        for (idx, block) in self.blocks.iter_mut().enumerate() {
            let input = match block {
                AnthropicContentBlock::ToolUse { input, .. }
                | AnthropicContentBlock::ServerToolUse { input, .. } => input,
                _ => continue,
            };
            if let Some(partial) = self.partial_inputs.remove(&idx)
                && !partial.is_empty()
            {
                *input = serde_json::from_str(&partial).map_err(|e| {
                    ProviderError::Protocol(format!("Invalid tool input JSON: {e}"))
                })?;
            }
        }
        Ok((self.blocks, self.usage))
    }
}

#[async_trait]
impl SseEventHandler for AnthropicStreamHandler {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError> {
        let ev_type = require_str(&json, "type")?;
        match ev_type {
            "message_start" => {
                let u = require_value(require_value(&json, "message")?, "usage")?;
                self.usage.seen = true;
                self.usage.input_tokens = require_u64(u, "input_tokens")? as u32;
                self.usage.cache_creation_input_tokens =
                    u["cache_creation_input_tokens"].as_u64().map(|v| v as u32);
                self.usage.cache_read_input_tokens =
                    u["cache_read_input_tokens"].as_u64().map(|v| v as u32);
            }
            "content_block_start" => {
                let idx = require_u64(&json, "index")? as usize;
                let cb = require_value(&json, "content_block")?;
                let block_type = require_str(cb, "type")?;
                let block = match block_type {
                    "thinking" => AnthropicContentBlock::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                    "text" => AnthropicContentBlock::Text {
                        // 官方规范中起始 text 恒为空串，允许省略
                        text: cb["text"].as_str().unwrap_or_default().to_string(),
                        citations: Vec::new(),
                    },
                    "tool_use" => AnthropicContentBlock::ToolUse {
                        id: require_str(cb, "id")?.to_string(),
                        name: require_str(cb, "name")?.to_string(),
                        input: serde_json::json!({}),
                    },
                    "server_tool_use" => {
                        self.partial_inputs.insert(idx, String::new());
                        AnthropicContentBlock::ServerToolUse {
                            id: require_str(cb, "id")?.to_string(),
                            name: require_str(cb, "name")?.to_string(),
                            input: serde_json::json!({}),
                        }
                    }
                    // server tool 结果块整块到达（执行期间流暂停，无增量 delta）
                    "web_search_tool_result" | "web_fetch_tool_result" => {
                        let tool_use_id = require_str(cb, "tool_use_id")?.to_string();
                        let raw_content = require_value(cb, "content")?;
                        if block_type == "web_search_tool_result" {
                            let content: AnthropicSearchResultContent =
                                from_value(raw_content, "web_search_tool_result content")?;
                            let input = self.server_use_input(&tool_use_id)?;
                            let call = project_search_call(&tool_use_id, &input, &content)?;
                            self.forwarder.web_search_call(call).await;
                            AnthropicContentBlock::WebSearchToolResult {
                                tool_use_id,
                                content,
                            }
                        } else {
                            let content: AnthropicFetchResultContent =
                                from_value(raw_content, "web_fetch_tool_result content")?;
                            let input = self.server_use_input(&tool_use_id)?;
                            let call = project_fetch_call(&tool_use_id, &input, &content)?;
                            self.forwarder.web_search_call(call).await;
                            AnthropicContentBlock::WebFetchToolResult {
                                tool_use_id,
                                content,
                            }
                        }
                    }
                    "redacted_thinking" => AnthropicContentBlock::RedactedThinking {
                        data: require_str(cb, "data")?.to_string(),
                    },
                    other => {
                        return Err(ProviderError::Protocol(format!(
                            "Unsupported Anthropic content block type '{other}'"
                        )));
                    }
                };
                self.ensure_block_slots(idx + 1);
                self.blocks[idx] = block;
            }
            "content_block_delta" => {
                let idx = require_u64(&json, "index")? as usize;
                let delta = require_value(&json, "delta")?;
                let delta_type = require_str(delta, "type")?;
                match delta_type {
                    "thinking_delta" => {
                        let t = require_str(delta, "thinking")?;
                        match self.blocks.get_mut(idx) {
                            Some(AnthropicContentBlock::Thinking { thinking, .. }) => {
                                thinking.push_str(t)
                            }
                            _ => {
                                return Err(ProviderError::Protocol(format!(
                                    "thinking_delta on non-thinking block {idx}"
                                )));
                            }
                        }
                        self.forwarder.thought(t).await;
                    }
                    "text_delta" => {
                        let t = require_str(delta, "text")?;
                        match self.blocks.get_mut(idx) {
                            Some(AnthropicContentBlock::Text { text, .. }) => text.push_str(t),
                            _ => {
                                return Err(ProviderError::Protocol(format!(
                                    "text_delta on non-text block {idx}"
                                )));
                            }
                        }
                        self.forwarder.text(&idx.to_string(), t).await;
                    }
                    // 引用以增量事件逐条下发，追加至当前 text 块的 citations 列表
                    "citations_delta" => {
                        let raw = require_value(delta, "citation")?;
                        let citation: AnthropicCitation =
                            from_value(raw, "citation in citations_delta")?;
                        match self.blocks.get_mut(idx) {
                            Some(AnthropicContentBlock::Text { citations, .. }) => {
                                citations.push(citation.clone())
                            }
                            _ => {
                                return Err(ProviderError::Protocol(format!(
                                    "citations_delta on non-text block {idx}"
                                )));
                            }
                        }
                        self.forwarder
                            .citation(&idx.to_string(), project_annotation(&citation))
                            .await;
                    }
                    "signature_delta" => {
                        let sig = require_str(delta, "signature")?;
                        match self.blocks.get_mut(idx) {
                            Some(AnthropicContentBlock::Thinking { signature, .. }) => {
                                *signature = Some(sig.to_string());
                            }
                            _ => {
                                return Err(ProviderError::Protocol(format!(
                                    "signature_delta on non-thinking block {idx}"
                                )));
                            }
                        }
                    }
                    "input_json_delta" => {
                        let pj = require_str(delta, "partial_json")?;
                        match self.blocks.get_mut(idx) {
                            Some(
                                AnthropicContentBlock::ToolUse { .. }
                                | AnthropicContentBlock::ServerToolUse { .. },
                            ) => {
                                self.partial_inputs.entry(idx).or_default().push_str(pj);
                            }
                            _ => {
                                return Err(ProviderError::Protocol(format!(
                                    "input_json_delta on non-tool_use block {idx}"
                                )));
                            }
                        }
                    }
                    other => {
                        return Err(ProviderError::Protocol(format!(
                            "Unsupported Anthropic delta type '{other}'"
                        )));
                    }
                }
            }
            "message_delta" => {
                if let Some(sr) = json.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                    self.usage.stop_reason = Some(sr.to_string());
                }
                if let Some(u) = json.get("usage") {
                    self.usage.seen = true;
                    self.usage.output_tokens = require_u64(u, "output_tokens")? as u32;
                }
            }
            "message_stop" | "ping" => {}
            "error" => {
                let msg = json
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .ok_or_else(|| {
                        ProviderError::Protocol("Missing 'error.message' in error event".into())
                    })?;
                return Err(ProviderError::Protocol(format!("Stream error: {msg}")));
            }
            // 未知顶层事件类型按前向兼容语义容忍（信封完整，不涉及内容丢失）
            _ => {}
        }
        Ok(())
    }
}
