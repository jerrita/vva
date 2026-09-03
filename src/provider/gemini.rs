use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

use crate::model::{
    Annotation, ContentPart, GeminiCodeExecutionResult, GeminiSafetyRating, GeminiTurnMeta,
    Message, Role, SearchStatus, Session, TokenUsage, ToolCall, ToolDefinition, TurnMeta,
    WebSearchCall, WebSearchSource,
};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};

pub struct GeminiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 将 Session 转换为 Gemini API 的 contents 与 tools 结构
    pub fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> serde_json::Value {
        let mut gemini_contents = Vec::new();

        for turn in &session.turns {
            for msg in &turn.messages {
                let gemini_role = match msg.role {
                    Role::Assistant => "model",
                    Role::User | Role::System => "user",
                    Role::Tool => "user",
                };

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
                        ContentPart::Image { source, .. } => {
                            if let crate::model::MediaSource::Base64 { data, mime_type } = source {
                                parts_arr.push(serde_json::json!({
                                    "inline_data": {
                                        "mime_type": mime_type,
                                        "data": data
                                    }
                                }));
                            }
                        }
                        ContentPart::ToolResult(res) => {
                            parts_arr.push(serde_json::json!({
                                "functionResponse": {
                                    "name": res.tool_call_id,
                                    "response": {
                                        "content": res.content,
                                        "is_error": res.is_error
                                    }
                                }
                            }));
                        }
                        _ => {}
                    }
                }

                if !parts_arr.is_empty() {
                    gemini_contents.push(serde_json::json!({
                        "role": gemini_role,
                        "parts": parts_arr,
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "contents": gemini_contents,
        });

        let mut tools_list: Vec<serde_json::Value> = Vec::new();

        // 核心原生优势：开启 Google 官方实时 Grounding
        if options.enable_web_search {
            tools_list.push(serde_json::json!({
                "google_search": {}
            }));
        }

        if !tools.is_empty() {
            let mut function_declarations = Vec::new();
            for t in tools {
                function_declarations.push(serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }));
            }
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
        if gen_config.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
            body["generationConfig"] = gen_config;
        }

        body
    }

    /// 解析 Gemini 响应，将 groundingMetadata 映射为 Responses 风格的 WebSearchCall 与 Annotation
    pub fn parse_response(&self, json: serde_json::Value) -> Result<ProviderResponse, ProviderError> {
        let candidate = json["candidates"]
            .as_array()
            .and_then(|c| c.first())
            .ok_or_else(|| ProviderError::Protocol("Empty candidates in Gemini response".into()))?;

        let finish_reason = candidate["finishReason"].as_str().map(|s| s.to_string());

        // 提取 Grounding 来源与文本引用
        let mut search_call: Option<WebSearchCall> = None;
        let mut citations: Vec<Annotation> = Vec::new();

        if let Some(gm) = candidate.get("groundingMetadata") {
            let mut search_sources = Vec::new();

            if let Some(chunks) = gm["groundingChunks"].as_array() {
                for chunk in chunks {
                    if let Some(web) = chunk.get("web") {
                        let uri = web["uri"].as_str().unwrap_or_default().to_string();
                        let title = web["title"].as_str().unwrap_or_default().to_string();
                        search_sources.push(WebSearchSource {
                            url: uri,
                            title,
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

            let combined_query = queries.join(" | ");
            if !search_sources.is_empty() || !queries.is_empty() {
                search_call = Some(WebSearchCall {
                    id: format!("gemini_search_{}", uuid::Uuid::new_v4().simple()),
                    query: combined_query,
                    status: SearchStatus::Completed,
                    sources: search_sources.clone(),
                });
            }

            // 映射 GroundingSupports 为标准的 UrlCitation
            if let Some(supports) = gm["groundingSupports"].as_array() {
                for sup in supports {
                    let seg = &sup["segment"];
                    let start_idx = seg["startIndex"].as_u64().unwrap_or(0) as usize;
                    let end_idx = seg["endIndex"].as_u64().unwrap_or(0) as usize;

                    if let Some(indices) = sup["groundingChunkIndices"].as_array() {
                        for idx_val in indices {
                            if let Some(idx) = idx_val.as_u64().map(|v| v as usize) {
                                if let Some(src) = search_sources.get(idx) {
                                    citations.push(Annotation::UrlCitation {
                                        url: src.url.clone(),
                                        title: src.title.clone(),
                                        start_index: start_idx,
                                        end_index: end_idx,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut parts = Vec::new();

        // 优先放入检索行为记录
        if let Some(sc) = search_call {
            parts.push(ContentPart::WebSearchCall(sc));
        }

        if let Some(raw_parts) = candidate["content"]["parts"].as_array() {
            for p in raw_parts {
                if p["thought"].as_bool() == Some(true) {
                    let thought_text = p["text"].as_str().unwrap_or_default().to_string();
                    parts.push(ContentPart::Reasoning {
                        content: thought_text,
                        signature: None,
                    });
                } else if let Some(text) = p["text"].as_str() {
                    parts.push(ContentPart::Text {
                        text: text.to_string(),
                        annotations: citations.clone(),
                    });
                } else if let Some(fc) = p.get("functionCall") {
                    let call_name = fc["name"].as_str().unwrap_or_default().to_string();
                    let args_str = fc["args"].to_string();
                    let call_id = format!("call_{}", uuid::Uuid::new_v4().simple());
                    parts.push(ContentPart::ToolCall(ToolCall {
                        id: call_id,
                        name: call_name,
                        arguments: args_str,
                    }));
                }
            }
        }

        // 安全评分
        let mut safety_ratings = Vec::new();
        if let Some(sr_arr) = candidate["safetyRatings"].as_array() {
            for sr in sr_arr {
                safety_ratings.push(GeminiSafetyRating {
                    category: sr["category"].as_str().unwrap_or_default().to_string(),
                    probability: sr["probability"].as_str().unwrap_or_default().to_string(),
                });
            }
        }

        // 原生代码执行
        let mut code_execution_result = None;
        if let Some(ce) = candidate.get("codeExecutionResult") {
            code_execution_result = Some(GeminiCodeExecutionResult {
                outcome: ce["outcome"].as_str().unwrap_or_default().to_string(),
                output: ce["output"].as_str().unwrap_or_default().to_string(),
            });
        }

        let gemini_turn_meta = GeminiTurnMeta {
            finish_reason,
            safety_ratings,
            code_execution_result,
        };

        let mut usage = None;
        if let Some(um) = json.get("usageMetadata") {
            let prompt = um["promptTokenCount"].as_u64().unwrap_or(0) as u32;
            let completion = um["candidatesTokenCount"].as_u64().unwrap_or(0) as u32;
            let total = um["totalTokenCount"].as_u64().unwrap_or(0) as u32;
            usage = Some(TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: total,
                cached_tokens: None,
                reasoning_tokens: None,
            });
        }

        let mut turn_meta = TurnMeta::default();
        turn_meta.gemini = Some(gemini_turn_meta);

        let message = Message::assistant(parts);

        Ok(ProviderResponse {
            messages: vec![message],
            usage,
            meta: turn_meta,
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
            supports_prompt_caching: false,
            supports_thinking_signature: false,
            supports_stateful_response_id: false,
        }
    }

    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, options.model, self.api_key
        );
        let body = self.build_request_body(session, tools, options);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let res = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = res.status();
        if !status.is_success() {
            let body_text = res.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError {
                status,
                body: body_text,
            });
        }

        let json_resp: serde_json::Value = res.json().await?;
        self.parse_response(json_resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

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

        let parsed = provider.parse_response(mock_gemini_response).expect("parse response");
        let msg = &parsed.messages[0];

        // 验证结构体解析：包含了 WebSearchCall, Reasoning, Text (with Annotation)
        assert_eq!(msg.parts.len(), 3);

        // 1. 验证 WebSearchCall 统一建模
        match &msg.parts[0] {
            ContentPart::WebSearchCall(call) => {
                assert_eq!(call.query, "Rust 2024 edition release announcement");
                assert_eq!(call.sources.len(), 1);
                assert_eq!(call.sources[0].url, "https://blog.rust-lang.org/2024/edition.html");
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
                if let Annotation::UrlCitation { url, start_index, end_index, .. } = &annotations[0] {
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
}
