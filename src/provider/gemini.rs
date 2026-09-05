use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::common::{RoleBlockMerger, require_str, require_u64};
use super::http::{SseEventHandler, post_json, post_sse};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};
use crate::event::AgentEvent;
use crate::model::{
    Annotation, ContentPart, GeminiCodeExecutionResult, GeminiSafetyRating, GeminiTurnMeta,
    Message, Role, SearchStatus, Session, TokenUsage, ToolCall, ToolDefinition, TurnMeta,
    WebSearchCall, WebSearchSource,
};

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
        headers.insert(
            "x-goog-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
        Ok(headers)
    }

    /// 将 Session 转换为 Gemini API 的 contents 与 tools 结构
    fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<serde_json::Value, ProviderError> {
        // Gemini 要求连续同角色内容聚合进同一条 content，仅与 Anthropic 的字段名不同
        let mut merger = RoleBlockMerger::new("parts");
        let mut system_instruction_parts = Vec::new();

        for turn in &session.turns {
            for msg in &turn.messages {
                match turn.role {
                    Role::System => {
                        system_instruction_parts.push(serde_json::json!({
                            "text": msg.text_content()
                        }));
                    }
                    Role::User => {
                        let mut parts_arr = Vec::new();
                        for part in &msg.parts {
                            match part {
                                ContentPart::Text { text, .. } => {
                                    parts_arr.push(serde_json::json!({ "text": text }));
                                }
                                ContentPart::Reasoning { content, .. } => {
                                    parts_arr.push(serde_json::json!({
                                        "thought": true,
                                        "text": content
                                    }));
                                }
                                ContentPart::Image {
                                    source: crate::model::MediaSource::Base64 { data, mime_type },
                                    ..
                                } => {
                                    parts_arr.push(serde_json::json!({
                                        "inline_data": {
                                            "mime_type": mime_type,
                                            "data": data
                                        }
                                    }));
                                }
                                _ => {}
                            }
                        }
                        merger.push("user", parts_arr);
                    }
                    Role::Assistant => {
                        let mut parts_arr = Vec::new();
                        for part in &msg.parts {
                            match part {
                                ContentPart::Text { text, .. } => {
                                    parts_arr.push(serde_json::json!({ "text": text }));
                                }
                                ContentPart::Reasoning { content, .. } => {
                                    parts_arr.push(serde_json::json!({
                                        "thought": true,
                                        "text": content
                                    }));
                                }
                                ContentPart::ToolCall(call) => {
                                    let args: serde_json::Value =
                                        serde_json::from_str(&call.arguments).map_err(|e| {
                                            ProviderError::Protocol(format!(
                                                "Invalid tool call '{}' arguments JSON: {e}",
                                                call.name
                                            ))
                                        })?;
                                    parts_arr.push(serde_json::json!({
                                        "functionCall": {
                                            "name": call.name,
                                            "args": args
                                        }
                                    }));
                                }
                                _ => {}
                            }
                        }
                        merger.push("model", parts_arr);
                    }
                    Role::Tool => {
                        let mut parts_arr = Vec::new();
                        for part in &msg.parts {
                            if let ContentPart::ToolResult(res) = part {
                                // 查找该 tool_call_id 对应的实际函数名称（缺失即会话数据损坏，按协议错误上报）
                                let function_name = session
                                    .find_tool_call(&res.tool_call_id)
                                    .map(|c| c.name.clone())
                                    .ok_or_else(|| {
                                        ProviderError::Protocol(format!(
                                            "No ToolCall '{}' found for functionResponse",
                                            res.tool_call_id
                                        ))
                                    })?;

                                parts_arr.push(serde_json::json!({
                                    "functionResponse": {
                                        "name": function_name,
                                        "response": {
                                            "content": res.content,
                                            "is_error": res.is_error
                                        }
                                    }
                                }));
                            }
                        }
                        merger.push("user", parts_arr);
                    }
                }
            }
        }

        let mut body = serde_json::json!({
            "contents": merger.into_messages(),
        });

        if !system_instruction_parts.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": system_instruction_parts
            });
        }

        let mut tools_list: Vec<serde_json::Value> = Vec::new();

        // 核心原生优势：开启 Google 官方实时 Grounding
        if options.enable_web_search {
            tools_list.push(serde_json::json!({
                "google_search": {}
            }));
        }

        if !tools.is_empty() {
            let function_declarations: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| serde_json::json!({"name": t.name, "description": t.description, "parameters": t.parameters}))
                .collect();
            tools_list.push(serde_json::json!({
                "function_declarations": function_declarations
            }));
        }

        if !tools_list.is_empty() {
            body["tools"] = serde_json::Value::Array(tools_list);
        }

        let mut gen_config = serde_json::json!({});
        if let Some(temp) = options.temperature {
            gen_config["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_tokens) = options.max_tokens {
            gen_config["maxOutputTokens"] = serde_json::json!(max_tokens);
        }
        if gen_config
            .as_object()
            .map(|m| !m.is_empty())
            .unwrap_or(false)
        {
            body["generationConfig"] = gen_config;
        }

        Ok(body)
    }

    /// 解析 Gemini 响应，将 groundingMetadata 映射为 Responses 风格的 WebSearchCall 与 Annotation
    fn parse_response(&self, json: serde_json::Value) -> Result<ProviderResponse, ProviderError> {
        if json["candidates"]
            .as_array()
            .and_then(|c| c.first())
            .is_none()
        {
            return Err(ProviderError::Protocol(
                "Empty candidates in Gemini response".into(),
            ));
        }

        let mut accumulator = GeminiAccumulator::new(None);
        accumulator.merge_response(&json)?;
        let (parts, meta, usage) = accumulator.finish();

        Ok(ProviderResponse {
            messages: vec![Message::new(parts)],
            usage,
            meta,
        })
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
        self.parse_response(response)
    }

    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError> {
        let body = self.build_request_body(session, tools, options)?;

        let mut handler = GeminiAccumulator::new(event_sender);
        post_sse(
            &self.endpoint_url(&options.model, true),
            self.auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;
        let (parts, meta, usage) = handler.finish();

        Ok(ProviderResponse {
            messages: vec![Message::new(parts)],
            usage,
            meta,
        })
    }
}

/// Gemini GenerateContentResponse 的增量累积器。
/// 非流式解析与 SSE 流式解析共用同一实现：每次合并一个完整 response JSON
/// （Gemini 流式每个事件均为完整响应结构而非增量 delta），
/// 流式场景额外实时转发 Thought / Text / Citation 增量事件。
struct GeminiAccumulator {
    sender: Option<mpsc::Sender<AgentEvent>>,
    /// 合并期间暂存的增量事件：同步合并逻辑无法 await，由异步 on_event 统一按背压语义转发
    pending_events: Vec<AgentEvent>,
    thought: String,
    text: String,
    tool_calls: Vec<ToolCall>,
    search_call: Option<WebSearchCall>,
    citations: Vec<Annotation>,
    finish_reason: Option<String>,
    safety_ratings: Vec<GeminiSafetyRating>,
    code_execution_result: Option<GeminiCodeExecutionResult>,
    usage: Option<TokenUsage>,
}

impl GeminiAccumulator {
    fn new(sender: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self {
            sender,
            pending_events: Vec::new(),
            thought: String::new(),
            text: String::new(),
            tool_calls: Vec::new(),
            search_call: None,
            citations: Vec::new(),
            finish_reason: None,
            safety_ratings: Vec::new(),
            code_execution_result: None,
            usage: None,
        }
    }

    /// 合并单个 GenerateContentResponse（流式 chunk 或完整响应）；
    /// 必填字段缺失或类型不符直接报协议错误
    fn merge_response(&mut self, json: &serde_json::Value) -> Result<(), ProviderError> {
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
            self.usage = Some(parse_usage_metadata(um)?);
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
                                    title: src.title.clone(),
                                    url: src.url.clone(),
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
                self.pending_events.push(AgentEvent::Text(text.to_string()));
            } else if let Some(fc) = p.get("functionCall") {
                self.tool_calls.push(ToolCall {
                    id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                    name: require_str(fc, "name")?.to_string(),
                    arguments: fc
                        .get("args")
                        .ok_or_else(|| {
                            ProviderError::Protocol("Missing 'args' in functionCall".into())
                        })?
                        .to_string(),
                });
            }
        }
        Ok(())
    }

    /// 组装为统一的领域内容块（检索记录 → 思考 → 正文 → 工具调用）与轮次元数据
    fn finish(self) -> (Vec<ContentPart>, TurnMeta, Option<TokenUsage>) {
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

        (parts, meta, self.usage)
    }
}

#[async_trait]
impl SseEventHandler for GeminiAccumulator {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError> {
        self.merge_response(&json)?;
        let pending = std::mem::take(&mut self.pending_events);
        if let Some(tx) = &self.sender {
            for ev in pending {
                let _ = tx.send(ev).await;
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::path::PathBuf;

    #[test]
    fn test_gemini_grounding_metadata_projection() {
        let provider = GeminiProvider::new("test-api-key");
        let mock_gemini_response = serde_json::json!({
            "candidates": [
                {
                    "content": {
                        "parts": [
                            {
                                "thought": true,
                                "text": "I need to check the official Rust edition 2024 guide."
                            },
                            {
                                "text": "According to the official announcement, Rust 2024 is the latest edition."
                            }
                        ],
                        "role": "model"
                    },
                    "finishReason": "STOP",
                    "groundingMetadata": {
                        "webSearchQueries": [
                            "Rust 2024 edition release announcement"
                        ],
                        "groundingChunks": [
                            {
                                "web": {
                                    "uri": "https://blog.rust-lang.org/2024/edition.html",
                                    "title": "Rust 2024 Released"
                                }
                            }
                        ],
                        "groundingSupports": [
                            {
                                "segment": {
                                    "startIndex": 0,
                                    "endIndex": 40,
                                    "text": "According to the official announcement"
                                },
                                "groundingChunkIndices": [0]
                            }
                        ]
                    },
                    "safetyRatings": [
                        {
                            "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                            "probability": "NEGLIGIBLE"
                        }
                    ]
                }
            ],
            "usageMetadata": {
                "promptTokenCount": 200,
                "candidatesTokenCount": 85,
                "totalTokenCount": 285
            }
        });

        let parsed = provider
            .parse_response(mock_gemini_response)
            .expect("parse response");
        let msg = &parsed.messages[0];

        // 验证结构体解析：包含了 WebSearchCall, Reasoning, Text (with Annotation)
        assert_eq!(msg.parts.len(), 3);

        // 1. 验证 WebSearchCall 统一建模
        match &msg.parts[0] {
            ContentPart::WebSearchCall(call) => {
                assert_eq!(call.query, "Rust 2024 edition release announcement");
                assert_eq!(call.sources.len(), 1);
                assert_eq!(
                    call.sources[0].url,
                    "https://blog.rust-lang.org/2024/edition.html"
                );
                assert_eq!(call.sources[0].title, "Rust 2024 Released");
            }
            _ => panic!("Expected WebSearchCall"),
        }

        // 2. 验证 thought 映射为 Reasoning
        match &msg.parts[1] {
            ContentPart::Reasoning { content, .. } => {
                assert!(content.contains("official Rust edition"));
            }
            _ => panic!("Expected Reasoning"),
        }

        // 3. 验证文本与 UrlCitation 注脚投影
        match &msg.parts[2] {
            ContentPart::Text { text, annotations } => {
                assert!(text.contains("Rust 2024 is the latest edition"));
                assert_eq!(annotations.len(), 1);
                if let Annotation::UrlCitation {
                    url,
                    start_index,
                    end_index,
                    ..
                } = &annotations[0]
                {
                    assert_eq!(url, "https://blog.rust-lang.org/2024/edition.html");
                    assert_eq!(*start_index, 0);
                    assert_eq!(*end_index, 40);
                } else {
                    panic!("Expected UrlCitation");
                }
            }
            _ => panic!("Expected Text"),
        }

        // 验证元数据
        let gemini_meta = parsed.meta.gemini.unwrap();
        assert_eq!(gemini_meta.finish_reason.as_deref(), Some("STOP"));
        assert_eq!(gemini_meta.safety_ratings.len(), 1);
    }

    #[test]
    fn test_gemini_system_instruction_and_tool_call_response_mapping() {
        let provider = GeminiProvider::new("test-key")
            .with_base_url("https://generativelanguage.googleapis.com/v1beta/models");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // System 指令
        session.append_turn(Turn::new(
            Role::System,
            vec![Message::text("You are a helpful assistant.")],
        ));
        // User 问题
        session.append_turn(Turn::user_turn(Message::text("List files")));

        // Assistant 调用 bash 工具
        let asst_msg = Message::new(vec![ContentPart::ToolCall(ToolCall {
            id: "call_abc123".into(),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        })]);
        session.append_turn(Turn::assistant_turn(vec![asst_msg]));

        // Tool 返回执行结果
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                tool_call_id: "call_abc123".into(),
                content: "file1.txt\nfile2.txt".into(),
                is_error: false,
            })],
        ));

        let options = CompletionOptions {
            model: "gemini-2.0-flash".into(),
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");

        // 验证 systemInstruction 独立提取
        let sys_inst = &body["systemInstruction"]["parts"][0]["text"];
        assert_eq!(sys_inst, "You are a helpful assistant.");

        // 验证 contents 角色交替与结构：user -> model (functionCall) -> user (functionResponse)
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");

        // 验证 model 的 functionCall
        let fc = &contents[1]["parts"][0]["functionCall"];
        assert_eq!(fc["name"], "bash");
        assert_eq!(fc["args"]["command"], "ls");

        // 验证 user 的 functionResponse 函数名正确匹配为 bash，而非 call_abc123
        let fr = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "bash");
        assert_eq!(fr["response"]["content"], "file1.txt\nfile2.txt");
    }

    #[test]
    fn test_gemini_streaming_accumulator_matches_non_streaming_parse() {
        // 流式累积器与非流式解析共用 GeminiAccumulator：
        // 相同的 response 结构分多个 chunk 合并后，结果必须与一次性解析一致
        let provider = GeminiProvider::new("test-key");
        let chunk1 = serde_json::json!({
            "candidates": [
                {
                    "content": {
                        "parts": [
                            { "thought": true, "text": "Analyzing " },
                            { "text": "Rust 2024 " }
                        ]
                    }
                }
            ],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 10,
                "totalTokenCount": 110
            }
        });
        let chunk2 = serde_json::json!({
            "candidates": [
                {
                    "content": {
                        "parts": [
                            { "text": "is the latest edition." }
                        ]
                    },
                    "finishReason": "STOP",
                    "groundingMetadata": {
                        "webSearchQueries": ["rust edition"],
                        "groundingChunks": [
                            { "web": { "uri": "https://blog.rust-lang.org", "title": "Rust Blog" } }
                        ],
                        "groundingSupports": [
                            {
                                "segment": { "startIndex": 0, "endIndex": 10 },
                                "groundingChunkIndices": [0]
                            }
                        ]
                    }
                }
            ],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 25,
                "totalTokenCount": 125
            }
        });

        let mut streaming = GeminiAccumulator::new(None);
        streaming.merge_response(&chunk1).expect("merge chunk1");
        streaming.merge_response(&chunk2).expect("merge chunk2");
        let (stream_parts, stream_meta, stream_usage) = streaming.finish();

        let full = serde_json::json!({
            "candidates": [
                {
                    "content": {
                        "parts": [
                            { "thought": true, "text": "Analyzing " },
                            { "text": "Rust 2024 is the latest edition." }
                        ]
                    },
                    "finishReason": "STOP",
                    "groundingMetadata": {
                        "webSearchQueries": ["rust edition"],
                        "groundingChunks": [
                            { "web": { "uri": "https://blog.rust-lang.org", "title": "Rust Blog" } }
                        ],
                        "groundingSupports": [
                            {
                                "segment": { "startIndex": 0, "endIndex": 10 },
                                "groundingChunkIndices": [0]
                            }
                        ]
                    }
                }
            ],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 25,
                "totalTokenCount": 125
            }
        });
        let parsed = provider.parse_response(full).expect("parse full");
        let (full_parts, full_meta, full_usage) = {
            let msg = &parsed.messages[0];
            let meta = parsed.meta.clone();
            (msg.parts.clone(), meta, parsed.usage.clone())
        };

        // 检索调用 id 含随机 UUID，比较前先规范化
        let normalize = |mut parts: Vec<ContentPart>| {
            for part in &mut parts {
                if let ContentPart::WebSearchCall(call) = part {
                    call.id = "normalized".to_string();
                }
            }
            parts
        };
        assert_eq!(normalize(stream_parts), normalize(full_parts));
        assert_eq!(stream_meta, full_meta);
        assert_eq!(stream_usage, full_usage);
    }

    #[test]
    fn test_gemini_streaming_grounding_chunks_merge_not_overwrite() {
        // 回归：流式响应中多个 chunk 分别携带 groundingMetadata 时，
        // 检索来源与查询必须合并保留，不得整体覆盖
        let chunk1 = serde_json::json!({
            "candidates": [{
                "groundingMetadata": {
                    "webSearchQueries": ["query one"],
                    "groundingChunks": [
                        { "web": { "uri": "https://a.example.com", "title": "A" } }
                    ]
                }
            }]
        });
        let chunk2 = serde_json::json!({
            "candidates": [{
                "groundingMetadata": {
                    "webSearchQueries": ["query two"],
                    "groundingChunks": [
                        { "web": { "uri": "https://b.example.com", "title": "B" } }
                    ]
                }
            }]
        });

        let mut acc = GeminiAccumulator::new(None);
        acc.merge_response(&chunk1).expect("merge chunk1");
        acc.merge_response(&chunk2).expect("merge chunk2");
        let (parts, _, _) = acc.finish();

        match &parts[0] {
            ContentPart::WebSearchCall(call) => {
                assert_eq!(call.sources.len(), 2);
                assert_eq!(call.sources[0].url, "https://a.example.com");
                assert_eq!(call.sources[1].url, "https://b.example.com");
                assert!(call.query.contains("query one"));
                assert!(call.query.contains("query two"));
            }
            _ => panic!("Expected WebSearchCall"),
        }
    }
}
