use std::collections::HashSet;

use tokio::sync::mpsc;

use crate::event::AgentEvent;
use crate::model::{
    Annotation, ContentPart, GeminiCodeExecutionResult, GeminiSafetyRating, GeminiTurnMeta,
    Message, SearchStatus, TokenUsage, ToolCall, ToolDefinition, ToolInput, TurnMeta,
    WebSearchAction, WebSearchCall, WebSearchSource,
};
use crate::provider::common::{EventForwarder, freeform_call_input, require_str, require_u64};
use crate::provider::{ProviderError, ProviderResponse};

/// Gemini GenerateContentResponse 的增量累积器。
/// 非流式解析与 SSE 流式解析共用同一实现：每次合并一个完整 response JSON
/// （Gemini 流式每个事件均为完整响应结构而非增量 delta），
/// 流式场景额外实时转发 Thought / Text / Citation 增量事件。
#[derive(Default)]
pub(super) struct GeminiAccumulator {
    forwarder: EventForwarder,
    /// 合并期间暂存的增量事件：同步合并逻辑无法 await，由异步 on_event 统一按背压语义转发
    pending_events: Vec<AgentEvent>,
    thought: String,
    text: String,
    tool_calls: Vec<ToolCall>,
    /// 本次请求中的 Freeform 工具名：其 functionCall 参数还原为原始输入文本
    freeform_tools: HashSet<String>,
    search_call: Option<WebSearchCall>,
    citations: Vec<Annotation>,
    finish_reason: Option<String>,
    safety_ratings: Vec<GeminiSafetyRating>,
    code_execution_result: Option<GeminiCodeExecutionResult>,
    usage: Option<TokenUsage>,
    received_response: bool,
}

impl GeminiAccumulator {
    pub(super) fn new(sender: Option<mpsc::Sender<AgentEvent>>, tools: &[ToolDefinition]) -> Self {
        Self {
            forwarder: EventForwarder::new(sender),
            freeform_tools: tools
                .iter()
                .filter(|t| matches!(t.input, ToolInput::Freeform { .. }))
                .map(|t| t.name.clone())
                .collect(),
            ..Default::default()
        }
    }

    /// 按背压语义转发合并期间暂存的增量事件
    pub(super) async fn flush_events(&mut self) {
        for event in std::mem::take(&mut self.pending_events) {
            self.forwarder.send(event).await;
        }
    }

    /// 合并单个 GenerateContentResponse 流式 chunk；必填字段缺失或类型不符直接报协议错误
    pub(super) fn merge_response(&mut self, json: &serde_json::Value) -> Result<(), ProviderError> {
        self.merge_response_impl(json, false)
    }

    fn merge_response_impl(
        &mut self,
        json: &serde_json::Value,
        require_complete_usage: bool,
    ) -> Result<(), ProviderError> {
        self.received_response = true;
        if let Some(candidate) = json["candidates"].as_array().and_then(|arr| arr.first()) {
            if let Some(fr) = candidate["finishReason"].as_str() {
                self.finish_reason = Some(fr.to_string());
            }

            if let Some(sr_arr) = candidate["safetyRatings"].as_array() {
                for sr in sr_arr {
                    self.safety_ratings.push(GeminiSafetyRating {
                        category: require_str(sr, "category")?.to_string(),
                        probability: require_str(sr, "probability")?.to_string(),
                    });
                }
            }

            if let Some(ce) = candidate.get("codeExecutionResult") {
                self.code_execution_result = Some(GeminiCodeExecutionResult {
                    outcome: require_str(ce, "outcome")?.to_string(),
                    output: require_str(ce, "output")?.to_string(),
                });
            }

            // 处理 GroundingMetadata：检索行为与文本引用注脚
            if let Some(gm) = candidate.get("groundingMetadata") {
                self.merge_grounding(gm)?;
            }

            // 处理 Parts (Thought / Text / ToolCall)
            if let Some(raw_parts) = candidate["content"]["parts"].as_array() {
                self.merge_parts(raw_parts)?;
            }
        }

        if let Some(um) = json.get("usageMetadata") {
            // 流式中间分片可能只携带 trafficType，等完整计数到达后再更新用量
            let has_token_counts = [
                "promptTokenCount",
                "candidatesTokenCount",
                "totalTokenCount",
            ]
            .into_iter()
            .any(|field| um.get(field).is_some());
            if require_complete_usage || has_token_counts {
                self.usage = Some(parse_usage_metadata(um)?);
            }
        }
        Ok(())
    }

    fn merge_grounding(&mut self, gm: &serde_json::Value) -> Result<(), ProviderError> {
        let mut search_sources = Vec::new();
        if let Some(chunks) = gm["groundingChunks"].as_array() {
            for chunk in chunks {
                if let Some(web) = chunk.get("web") {
                    search_sources.push(WebSearchSource {
                        url: require_str(web, "uri")?.to_string(),
                        title: require_str(web, "title")?.to_string(),
                        snippet: None,
                    });
                }
            }
        }

        let queries: Vec<String> = gm["webSearchQueries"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|q| q.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        if !search_sources.is_empty() || !queries.is_empty() {
            // 流式响应中 groundingMetadata 会在多个 chunk 上分别下发，追加合并而非整体覆盖
            let joined = queries.join(" | ");
            match &mut self.search_call {
                Some(existing) => {
                    for src in &search_sources {
                        if !existing.sources.contains(src) {
                            existing.sources.push(src.clone());
                        }
                    }
                    if !joined.is_empty() && !existing.query.contains(&joined) {
                        existing.query = if existing.query.is_empty() {
                            joined
                        } else {
                            format!("{} | {}", existing.query, joined)
                        };
                    }
                }
                None => {
                    self.search_call = Some(WebSearchCall {
                        action: Some(WebSearchAction::Search),
                        id: format!("gemini_search_{}", uuid::Uuid::new_v4().simple()),
                        query: joined,
                        status: SearchStatus::Completed,
                        sources: search_sources.clone(),
                    });
                }
            }
        }

        if let Some(supports) = gm["groundingSupports"].as_array() {
            for sup in supports {
                let seg = &sup["segment"];
                let start_idx = seg["startIndex"].as_u64().unwrap_or(0) as usize;
                let end_idx = seg["endIndex"].as_u64().unwrap_or(0) as usize;

                if let Some(indices) = sup["groundingChunkIndices"].as_array() {
                    for idx_val in indices {
                        if let Some(idx) = idx_val.as_u64().map(|v| v as usize)
                            && let Some(src) = search_sources.get(idx)
                        {
                            let ann = Annotation::UrlCitation {
                                url: src.url.clone(),
                                title: src.title.clone(),
                                start_index: start_idx,
                                end_index: end_idx,
                            };
                            if !self.citations.contains(&ann) {
                                self.pending_events.push(AgentEvent::Citation {
                                    text_id: "text".into(),
                                    annotation: ann.clone(),
                                });
                                self.citations.push(ann);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn merge_parts(&mut self, raw_parts: &[serde_json::Value]) -> Result<(), ProviderError> {
        for p in raw_parts {
            if p["thought"].as_bool() == Some(true) {
                if let Some(t) = p["text"].as_str() {
                    self.thought.push_str(t);
                    self.pending_events.push(AgentEvent::Thought(t.to_string()));
                }
            } else if let Some(text) = p["text"].as_str() {
                self.text.push_str(text);
                self.pending_events.push(AgentEvent::Text {
                    text_id: "text".into(),
                    delta: text.to_string(),
                });
            } else if let Some(fc) = p.get("functionCall") {
                let name = require_str(fc, "name")?;
                let args = fc.get("args").ok_or_else(|| {
                    ProviderError::Protocol("Missing 'args' in functionCall".into())
                })?;
                let id = format!("call_{}", uuid::Uuid::new_v4().simple());
                self.tool_calls.push(if self.freeform_tools.contains(name) {
                    ToolCall::custom(id, name, freeform_call_input(name, args)?)
                } else {
                    ToolCall::new(id, name, args.to_string())
                });
            }
        }
        Ok(())
    }

    /// 解析非流式响应：groundingMetadata 映射为 Responses 风格的 WebSearchCall 与 Annotation
    pub(super) fn parse_response(
        json: &serde_json::Value,
        tools: &[ToolDefinition],
    ) -> Result<ProviderResponse, ProviderError> {
        if json["candidates"].as_array().is_none_or(|c| c.is_empty()) {
            return Err(ProviderError::Protocol(
                "Empty candidates in Gemini response".into(),
            ));
        }
        let mut accumulator = Self::new(None, tools);
        accumulator.merge_response_impl(json, true)?;
        Ok(accumulator.finish())
    }

    /// 空 SSE 流属于传输协议异常；收到分片后仍允许无正文的结束响应
    pub(super) fn finish_stream(self) -> Result<ProviderResponse, ProviderError> {
        if !self.received_response {
            return Err(ProviderError::Protocol(
                "Stream ended without a Gemini response".into(),
            ));
        }
        Ok(self.finish())
    }

    /// 组装为统一的领域内容块（检索记录 → 思考 → 正文 → 工具调用）与轮次元数据
    pub(super) fn finish(self) -> ProviderResponse {
        let mut parts = Vec::new();
        if let Some(sc) = self.search_call {
            parts.push(ContentPart::WebSearchCall(sc));
        }
        if !self.thought.is_empty() {
            parts.push(ContentPart::Reasoning {
                content: self.thought,
                signature: None,
            });
        }
        if !self.text.is_empty() {
            parts.push(ContentPart::Text {
                text: self.text,
                annotations: self.citations,
            });
        }
        for call in self.tool_calls {
            parts.push(ContentPart::ToolCall(call));
        }

        let meta = TurnMeta {
            gemini: Some(GeminiTurnMeta {
                finish_reason: self.finish_reason,
                safety_ratings: self.safety_ratings,
                code_execution_result: self.code_execution_result,
            }),
            ..Default::default()
        };

        ProviderResponse {
            messages: vec![Message::new(parts)],
            usage: self.usage,
            meta,
        }
    }
}

/// 解析 Gemini usageMetadata 为统一 TokenUsage（必填计数字段缺失即协议错误）
fn parse_usage_metadata(um: &serde_json::Value) -> Result<TokenUsage, ProviderError> {
    let prompt = require_u64(um, "promptTokenCount")? as u32;
    let completion = require_u64(um, "candidatesTokenCount")? as u32;
    let total = require_u64(um, "totalTokenCount")? as u32;
    let cached = um["cachedContentTokenCount"].as_u64().map(|v| v as u32);
    let reasoning = um["candidatesTokensDetails"]
        .as_array()
        .and_then(|arr| arr.iter().find(|d| d["modality"] == "THOUGHT"))
        .and_then(|d| d["tokenCount"].as_u64())
        .map(|v| v as u32);
    Ok(TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_tokens: cached,
        reasoning_tokens: reasoning,
    })
}
