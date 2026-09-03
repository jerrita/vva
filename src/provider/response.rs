use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

use crate::model::{
    Annotation, ContentPart, Message, ResponseTurnMeta, Role, SearchStatus, Session, TokenUsage,
    ToolCall, ToolDefinition, TurnMeta, WebSearchCall, WebSearchSource,
};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};

pub struct ResponseProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl ResponseProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 将 Session 与当前上下文转换为 Responses API 请求体
    pub fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> serde_json::Value {
        let prev_id = session.latest_openai_response_id();

        // 构造 tools 列表
        let mut api_tools: Vec<serde_json::Value> = Vec::new();
        if options.enable_web_search {
            api_tools.push(serde_json::json!({
                "type": "web_search_preview"
            }));
        }

        for tool in tools {
            api_tools.push(serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            }));
        }

        let mut input_items: Vec<serde_json::Value> = Vec::new();

        // 如果存在 previous_response_id，则只需要增量发送自该 response_id 之后产生的用户和工具消息
        let turns_to_send = if prev_id.is_some() {
            let mut found_prev = false;
            let mut delta = Vec::new();
            for turn in &session.turns {
                if found_prev {
                    delta.push(turn);
                } else if turn.openai_response_id() == prev_id {
                    found_prev = true;
                }
            }
            if found_prev && !delta.is_empty() {
                delta
            } else {
                session.turns.iter().collect()
            }
        } else {
            session.turns.iter().collect()
        };

        for turn in turns_to_send {
            for msg in &turn.messages {
                match msg.role {
                    Role::User => {
                        let mut content_arr = Vec::new();
                        for part in &msg.parts {
                            match part {
                                ContentPart::Text { text, .. } => {
                                    content_arr.push(serde_json::json!({
                                        "type": "input_text",
                                        "text": text,
                                    }));
                                }
                                ContentPart::Image { source, .. } => {
                                    if let crate::model::MediaSource::Url { url } = source {
                                        content_arr.push(serde_json::json!({
                                            "type": "input_image",
                                            "image_url": url,
                                        }));
                                    }
                                }
                                _ => {}
                            }
                        }
                        input_items.push(serde_json::json!({
                            "type": "message",
                            "role": "user",
                            "content": content_arr,
                        }));
                    }
                    Role::Tool => {
                        for part in &msg.parts {
                            if let ContentPart::ToolResult(res) = part {
                                input_items.push(serde_json::json!({
                                    "type": "function_call_output",
                                    "call_id": res.tool_call_id,
                                    "output": res.content,
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut body = serde_json::json!({
            "model": options.model,
            "input": input_items,
        });

        if let Some(prev) = prev_id {
            body["previous_response_id"] = serde_json::Value::String(prev.to_string());
        }

        if !api_tools.is_empty() {
            body["tools"] = serde_json::Value::Array(api_tools);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(max_tokens) = options.max_tokens {
            body["max_output_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(effort) = &options.reasoning_effort {
            body["reasoning_effort"] = serde_json::json!(effort);
        }

        body
    }

    /// 解析 Responses API 响应体
    pub fn parse_response(&self, json: serde_json::Value) -> Result<ProviderResponse, ProviderError> {
        let response_id = json["id"]
            .as_str()
            .ok_or_else(|| ProviderError::Protocol("Missing 'id' in Responses API output".into()))?
            .to_string();

        let status = json["status"].as_str().map(|s| s.to_string());
        let mut parts = Vec::new();

        if let Some(output_items) = json["output"].as_array() {
            for item in output_items {
                let item_type = item["type"].as_str().unwrap_or_default();
                match item_type {
                    "web_search_call" => {
                        let search_id = item["id"].as_str().unwrap_or_default().to_string();
                        let call_obj = &item["web_search_call"];
                        let query = call_obj["query"].as_str().unwrap_or_default().to_string();

                        let mut sources = Vec::new();
                        if let Some(srcs) = call_obj["sources"].as_array() {
                            for src in srcs {
                                sources.push(WebSearchSource {
                                    url: src["url"].as_str().unwrap_or_default().to_string(),
                                    title: src["title"].as_str().unwrap_or_default().to_string(),
                                    snippet: src["snippet"].as_str().map(|s| s.to_string()),
                                });
                            }
                        }

                        parts.push(ContentPart::WebSearchCall(WebSearchCall {
                            id: search_id,
                            query,
                            status: SearchStatus::Completed,
                            sources,
                        }));
                    }
                    "message" => {
                        if let Some(contents) = item["content"].as_array() {
                            for c in contents {
                                if c["type"].as_str() == Some("text") {
                                    let text = c["text"].as_str().unwrap_or_default().to_string();
                                    let mut annotations = Vec::new();

                                    if let Some(ann_arr) = c["annotations"].as_array() {
                                        for ann in ann_arr {
                                            if ann["type"].as_str() == Some("url_citation") {
                                                annotations.push(Annotation::UrlCitation {
                                                    url: ann["url"].as_str().unwrap_or_default().to_string(),
                                                    title: ann["title"].as_str().unwrap_or_default().to_string(),
                                                    start_index: ann["start_index"].as_u64().unwrap_or(0) as usize,
                                                    end_index: ann["end_index"].as_u64().unwrap_or(0) as usize,
                                                });
                                            }
                                        }
                                    }

                                    parts.push(ContentPart::Text { text, annotations });
                                }
                            }
                        }
                    }
                    "function_call" => {
                        let call_id = item["id"].as_str().unwrap_or_default().to_string();
                        let name = item["name"].as_str().unwrap_or_default().to_string();
                        let arguments = item["arguments"].as_str().unwrap_or("{}").to_string();

                        parts.push(ContentPart::ToolCall(ToolCall {
                            id: call_id,
                            name,
                            arguments,
                        }));
                    }
                    _ => {}
                }
            }
        }

        let mut usage = None;
        if let Some(u) = json.get("usage") {
            usage = Some(TokenUsage {
                prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
                cached_tokens: None,
                reasoning_tokens: None,
            });
        }

        let mut meta = TurnMeta::default();
        meta.response = Some(ResponseTurnMeta {
            response_id,
            previous_response_id: json["previous_response_id"].as_str().map(|s| s.to_string()),
            status,
            service_tier: json["service_tier"].as_str().map(|s| s.to_string()),
            system_fingerprint: json["system_fingerprint"].as_str().map(|s| s.to_string()),
            reasoning_effort: json["reasoning_effort"].as_str().map(|s| s.to_string()),
        });

        let message = Message::assistant(parts);

        Ok(ProviderResponse {
            messages: vec![message],
            usage,
            meta,
        })
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
            supports_prompt_caching: false,
            supports_thinking_signature: false,
            supports_stateful_response_id: true,
        }
    }

    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let body = self.build_request_body(session, tools, options);

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
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
    use std::path::PathBuf;

    #[test]
    fn test_response_provider_payload_construction_and_previous_id() {
        let provider = ResponseProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        let user_msg = Message::user("Check the weather in Tokyo");
        let user_turn = Turn::user_turn(user_msg, None);
        session.append_turn(user_turn);

        // 第一轮：无 previous_response_id
        let options = CompletionOptions {
            model: "gpt-4.5-preview".into(),
            enable_web_search: true,
            ..Default::default()
        };
        let body1 = provider.build_request_body(&session, &[], &options);
        assert!(body1.get("previous_response_id").is_none());
        assert_eq!(body1["tools"][0]["type"], "web_search_preview");

        // 模拟第一轮 Assistant 成功返回 response_id = "resp_turn_1"
        let mut asst_turn = Turn::assistant_turn(vec![Message::assistant(vec![ContentPart::text("It is sunny.")])], None);
        asst_turn.meta.response = Some(ResponseTurnMeta::new("resp_turn_1"));
        session.append_turn(asst_turn);

        // 用户提出第二轮需求
        let user_msg2 = Message::user("What about tomorrow?");
        session.append_turn(Turn::user_turn(user_msg2, None));

        // 第二轮：应自动提取 previous_response_id: "resp_turn_1"
        let body2 = provider.build_request_body(&session, &[], &options);
        assert_eq!(body2["previous_response_id"], "resp_turn_1");
        // 增量 input 只包含第二轮的用户提问
        let inputs = body2["input"].as_array().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["content"][0]["text"], "What about tomorrow?");
    }

    #[test]
    fn test_response_provider_parse_with_search_and_citations() {
        let provider = ResponseProvider::new("test-key");
        let mock_json = serde_json::json!({
            "id": "resp_98765",
            "status": "completed",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "call_s1",
                    "web_search_call": {
                        "query": "tokyo weather",
                        "status": "completed",
                        "sources": [
                            {
                                "url": "https://weather.com/tokyo",
                                "title": "Tokyo Weather Forecast",
                                "snippet": "Sunny 22C"
                            }
                        ]
                    }
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "text",
                            "text": "Tokyo is sunny with 22C today [1].",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://weather.com/tokyo",
                                    "title": "Tokyo Weather Forecast",
                                    "start_index": 30,
                                    "end_index": 33
                                }
                            ]
                        }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 150,
                "output_tokens": 40,
                "total_tokens": 190
            }
        });

        let parsed = provider.parse_response(mock_json).expect("parse response");
        assert_eq!(parsed.meta.response.unwrap().response_id, "resp_98765");
        let msg = &parsed.messages[0];
        assert_eq!(msg.parts.len(), 2);

        // 验证 WebSearchCall 映射
        match &msg.parts[0] {
            ContentPart::WebSearchCall(call) => {
                assert_eq!(call.id, "call_s1");
                assert_eq!(call.query, "tokyo weather");
                assert_eq!(call.sources.len(), 1);
                assert_eq!(call.sources[0].url, "https://weather.com/tokyo");
            }
            _ => panic!("Expected WebSearchCall"),
        }

        // 验证带 Annotation 的 Text 映射
        match &msg.parts[1] {
            ContentPart::Text { text, annotations } => {
                assert!(text.contains("Tokyo is sunny"));
                assert_eq!(annotations.len(), 1);
                if let Annotation::UrlCitation { start_index, end_index, .. } = annotations[0] {
                    assert_eq!(start_index, 30);
                    assert_eq!(end_index, 33);
                } else {
                    panic!("Expected UrlCitation");
                }
            }
            _ => panic!("Expected Text with annotations"),
        }
    }
}
