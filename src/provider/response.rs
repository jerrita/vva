use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::{
    CompletionOptions, Provider, ProviderCapabilities, ProviderDelta, ProviderError,
    ProviderResponse, sse::SseDecoder,
};
use crate::model::{
    Annotation, ContentPart, Message, ResponseOutputItem, ResponseTurnMeta, Role, SearchStatus,
    Session, TokenUsage, ToolCall, ToolDefinition, TurnMeta, WebSearchCall, WebSearchSource,
};

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
                "type": "web_search"
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
            if turn.role == Role::Assistant
                && let Some(response_meta) = &turn.meta.response
                && !response_meta.output_items.is_empty()
            {
                input_items.extend(
                    response_meta
                        .output_items
                        .iter()
                        .map(|item| serde_json::json!(item)),
                );
                continue;
            }

            for msg in &turn.messages {
                match msg.role {
                    Role::System => {
                        input_items.push(serde_json::json!({
                            "type": "message",
                            "role": "system",
                            "content": [{
                                "type": "input_text",
                                "text": msg.text_content(),
                            }],
                        }));
                    }
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
                                ContentPart::Image {
                                    source: crate::model::MediaSource::Url { url },
                                    ..
                                } => {
                                    content_arr.push(serde_json::json!({
                                        "type": "input_image",
                                        "image_url": url,
                                    }));
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
                    Role::Assistant => {
                        // 当无 previous_response_id 时（跨供应商切换或冷启动），需带上历史 Assistant 内容与 ToolCall
                        let mut content_arr = Vec::new();
                        for part in &msg.parts {
                            match part {
                                ContentPart::Text { text, .. } => {
                                    content_arr.push(serde_json::json!({
                                        "type": "output_text",
                                        "text": text,
                                    }));
                                }
                                ContentPart::ToolCall(call) => {
                                    input_items.push(serde_json::json!({
                                        "type": "function_call",
                                        "call_id": call.id,
                                        "name": call.name,
                                        "arguments": call.arguments,
                                    }));
                                }
                                _ => {}
                            }
                        }
                        if !content_arr.is_empty() {
                            input_items.push(serde_json::json!({
                                "type": "message",
                                "role": "assistant",
                                "content": content_arr,
                            }));
                        }
                    }
                    Role::Tool => {
                        // 确保所有待提交 output 的 function_call 均在 input_items 中声明（例如当 Assistant 轮次被 previous_response_id 截断时补齐）
                        for part in &msg.parts {
                            if let ContentPart::ToolResult(res) = part
                                && !input_items.iter().any(|it| {
                                    it["type"] == "function_call"
                                        && it["call_id"] == res.tool_call_id
                                })
                                && let Some(call) = session.find_tool_call(&res.tool_call_id)
                            {
                                input_items.push(serde_json::json!({
                                    "type": "function_call",
                                    "call_id": call.id,
                                    "name": call.name,
                                    "arguments": call.arguments,
                                }));
                            }
                        }
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
                }
            }
        }

        let mut body = serde_json::json!({
            "model": options.model,
            "input": input_items,
            "store": true,
            "prompt_cache_key": session.id,
        });

        let mut include = Vec::new();
        if options.reasoning_effort.is_some() {
            include.push(serde_json::json!("reasoning.encrypted_content"));
        }
        if options.enable_web_search {
            include.push(serde_json::json!("web_search_call.action.sources"));
        }
        if !include.is_empty() {
            body["include"] = serde_json::Value::Array(include);
        }

        if let Some(prev) = prev_id {
            body["previous_response_id"] = serde_json::Value::String(prev.to_string());
        }

        if !api_tools.is_empty() {
            body["tools"] = serde_json::Value::Array(api_tools);
        }

        // OpenAI Responses API 中开启 reasoning 时传 temperature 会报 400
        if let Some(temp) = options.temperature
            && options.reasoning_effort.is_none()
        {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(max_tokens) = options.max_tokens {
            body["max_output_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(effort) = &options.reasoning_effort
            && !effort.is_empty()
        {
            body["reasoning"] = serde_json::json!({
                "effort": effort
            });
        }

        body
    }

    /// 解析 Responses API 响应体
    /// 解析 Responses API 响应体
    pub fn parse_response(
        &self,
        json: serde_json::Value,
    ) -> Result<ProviderResponse, ProviderError> {
        let is_stored = json["store"].as_bool().ok_or_else(|| {
            ProviderError::Protocol("Missing 'store' in Responses API output".into())
        })?;

        let raw_id = json["id"].as_str().ok_or_else(|| {
            ProviderError::Protocol("Missing 'id' in Responses API output".into())
        })?;

        let response_id = if is_stored {
            Some(raw_id.to_string())
        } else {
            None
        };

        let status = json["status"]
            .as_str()
            .ok_or_else(|| {
                ProviderError::Protocol("Missing 'status' in Responses API output".into())
            })?
            .to_string();

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

        let mut parts = Vec::new();
        let mut last_msg_item_id = None;

        let output_items = json["output"].as_array().ok_or_else(|| {
            ProviderError::Protocol("Missing 'output' in Responses API output".into())
        })?;

        let mut native_output_items = Vec::with_capacity(output_items.len());
        for item in output_items {
            let native_item: ResponseOutputItem =
                serde_json::from_value(item.clone()).map_err(|e| {
                    ProviderError::Protocol(format!("Invalid Responses API output item: {e}"))
                })?;
            native_output_items.push(native_item);

            let item_type = item["type"]
                .as_str()
                .ok_or_else(|| ProviderError::Protocol("Missing 'type' in output item".into()))?;

            match item_type {
                "web_search_call" => {
                    let search_id = item["id"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::Protocol("Missing 'id' in web_search_call".into())
                        })?
                        .to_string();
                    let _search_status = item["status"].as_str().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'status' in web_search_call".into())
                    })?;
                    let action = item["action"].as_object().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'action' in web_search_call".into())
                    })?;
                    let action_type = action["type"].as_str().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'type' in web_search_call action".into())
                    })?;

                    let (query, sources) = match action_type {
                        "search" => {
                            let q = if let Some(q) = action.get("query").and_then(|v| v.as_str()) {
                                q.to_string()
                            } else if let Some(queries) =
                                action.get("queries").and_then(|v| v.as_array())
                            {
                                queries
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            } else {
                                String::new()
                            };

                            let mut srcs = Vec::new();
                            if let Some(sources_arr) =
                                action.get("sources").and_then(|v| v.as_array())
                            {
                                for src in sources_arr {
                                    let url = src["url"]
                                        .as_str()
                                        .ok_or_else(|| {
                                            ProviderError::Protocol(
                                                "Missing 'url' in web search source".into(),
                                            )
                                        })?
                                        .to_string();
                                    srcs.push(WebSearchSource {
                                        url: url.clone(),
                                        title: url,
                                        snippet: None,
                                    });
                                }
                            }
                            (q, srcs)
                        }
                        "open_page" => {
                            let url = action["url"]
                                .as_str()
                                .ok_or_else(|| {
                                    ProviderError::Protocol(
                                        "Missing 'url' in open_page action".into(),
                                    )
                                })?
                                .to_string();
                            (
                                url.clone(),
                                vec![WebSearchSource {
                                    url: url.clone(),
                                    title: url,
                                    snippet: None,
                                }],
                            )
                        }
                        _ => (String::new(), Vec::new()),
                    };

                    parts.push(ContentPart::WebSearchCall(WebSearchCall {
                        id: search_id,
                        query,
                        status: SearchStatus::Completed,
                        sources,
                    }));
                }
                "reasoning" => {
                    let _reasoning_id = item["id"].as_str().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'id' in reasoning output item".into())
                    })?;
                    let summary_arr = item["summary"].as_array().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'summary' in reasoning output item".into())
                    })?;

                    let mut reasoning_text = String::new();
                    for c in summary_arr {
                        let t = c["text"].as_str().ok_or_else(|| {
                            ProviderError::Protocol("Missing 'text' in summary_text item".into())
                        })?;
                        if !reasoning_text.is_empty() {
                            reasoning_text.push('\n');
                        }
                        reasoning_text.push_str(t);
                    }

                    if let Some(contents) = item.get("content").and_then(|v| v.as_array()) {
                        for c in contents {
                            let t = c["text"].as_str().ok_or_else(|| {
                                ProviderError::Protocol(
                                    "Missing 'text' in reasoning_text item".into(),
                                )
                            })?;
                            if !reasoning_text.is_empty() {
                                reasoning_text.push('\n');
                            }
                            reasoning_text.push_str(t);
                        }
                    }

                    if !reasoning_text.is_empty() {
                        parts.push(ContentPart::Reasoning {
                            content: reasoning_text,
                            signature: None,
                        });
                    }
                }
                "message" => {
                    let id = item["id"].as_str().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'id' in message output item".into())
                    })?;
                    if is_stored {
                        last_msg_item_id = Some(id.to_string());
                    }

                    let role = item["role"].as_str().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'role' in message output item".into())
                    })?;
                    if role != "assistant" {
                        return Err(ProviderError::Protocol(format!(
                            "Unexpected role in output message: {role}"
                        )));
                    }

                    let _msg_status = item["status"].as_str().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'status' in message output item".into())
                    })?;

                    let contents = item["content"].as_array().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'content' in message output item".into())
                    })?;

                    for c in contents {
                        let c_type = c["type"].as_str().ok_or_else(|| {
                            ProviderError::Protocol("Missing 'type' in message content part".into())
                        })?;
                        match c_type {
                            "output_text" => {
                                let text = c["text"]
                                    .as_str()
                                    .ok_or_else(|| {
                                        ProviderError::Protocol(
                                            "Missing 'text' in output_text content part".into(),
                                        )
                                    })?
                                    .to_string();
                                let ann_arr = c["annotations"].as_array().ok_or_else(|| {
                                    ProviderError::Protocol(
                                        "Missing 'annotations' in output_text content part".into(),
                                    )
                                })?;

                                let mut annotations = Vec::new();
                                for ann in ann_arr {
                                    let ann_type = ann["type"].as_str().ok_or_else(|| {
                                        ProviderError::Protocol(
                                            "Missing 'type' in annotation".into(),
                                        )
                                    })?;
                                    if ann_type == "url_citation" {
                                        let url = ann["url"]
                                            .as_str()
                                            .ok_or_else(|| {
                                                ProviderError::Protocol(
                                                    "Missing 'url' in url_citation annotation"
                                                        .into(),
                                                )
                                            })?
                                            .to_string();
                                        let title = ann["title"]
                                            .as_str()
                                            .ok_or_else(|| {
                                                ProviderError::Protocol(
                                                    "Missing 'title' in url_citation annotation"
                                                        .into(),
                                                )
                                            })?
                                            .to_string();
                                        let start_index = ann["start_index"]
                                            .as_u64()
                                            .ok_or_else(|| ProviderError::Protocol("Missing 'start_index' in url_citation annotation".into()))? as usize;
                                        let end_index = ann["end_index"]
                                            .as_u64()
                                            .ok_or_else(|| ProviderError::Protocol("Missing 'end_index' in url_citation annotation".into()))? as usize;
                                        annotations.push(Annotation::UrlCitation {
                                            url,
                                            title,
                                            start_index,
                                            end_index,
                                        });
                                    }
                                }
                                parts.push(ContentPart::Text { text, annotations });
                            }
                            "refusal" => {
                                let refusal = c["refusal"]
                                    .as_str()
                                    .ok_or_else(|| {
                                        ProviderError::Protocol(
                                            "Missing 'refusal' in refusal content part".into(),
                                        )
                                    })?
                                    .to_string();
                                parts.push(ContentPart::text(refusal));
                            }
                            _ => {}
                        }
                    }
                }
                "function_call" => {
                    let call_id = item["call_id"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::Protocol(
                                "Missing 'call_id' in function_call output item".into(),
                            )
                        })?
                        .to_string();
                    let name = item["name"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::Protocol(
                                "Missing 'name' in function_call output item".into(),
                            )
                        })?
                        .to_string();
                    let arguments = item["arguments"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::Protocol(
                                "Missing 'arguments' in function_call output item".into(),
                            )
                        })?
                        .to_string();

                    parts.push(ContentPart::ToolCall(ToolCall {
                        id: call_id,
                        name,
                        arguments,
                    }));
                }
                _ => {}
            }
        }

        let mut usage = None;
        if let Some(u) = json.get("usage")
            && !u.is_null()
        {
            let input_tokens = u["input_tokens"]
                .as_u64()
                .ok_or_else(|| ProviderError::Protocol("Missing 'input_tokens' in usage".into()))?
                as u32;
            let output_tokens = u["output_tokens"]
                .as_u64()
                .ok_or_else(|| ProviderError::Protocol("Missing 'output_tokens' in usage".into()))?
                as u32;
            let total_tokens = u["total_tokens"]
                .as_u64()
                .ok_or_else(|| ProviderError::Protocol("Missing 'total_tokens' in usage".into()))?
                as u32;

            let input_details = u["input_tokens_details"].as_object().ok_or_else(|| {
                ProviderError::Protocol("Missing 'input_tokens_details' in usage".into())
            })?;
            let cached = input_details["cached_tokens"].as_u64().ok_or_else(|| {
                ProviderError::Protocol("Missing 'cached_tokens' in input_tokens_details".into())
            })? as u32;

            let output_details = u["output_tokens_details"].as_object().ok_or_else(|| {
                ProviderError::Protocol("Missing 'output_tokens_details' in usage".into())
            })?;
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

        let mut message = Message::assistant(parts);
        if let Some(msg_id) = last_msg_item_id {
            message.meta.response = Some(crate::model::ResponseMessageMeta {
                item_id: Some(msg_id),
            });
        }

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
        let base = self.base_url.trim_end_matches('/');
        let url = if base.ends_with("/responses") {
            base.to_string()
        } else {
            format!("{base}/responses")
        };
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

    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        delta_sender: Option<mpsc::Sender<ProviderDelta>>,
    ) -> Result<ProviderResponse, ProviderError> {
        let base = self.base_url.trim_end_matches('/');
        let url = if base.ends_with("/responses") {
            base.to_string()
        } else {
            format!("{base}/responses")
        };
        let mut body = self.build_request_body(session, tools, options);
        body["stream"] = serde_json::json!(true);

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

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

        let mut stream = res.bytes_stream();
        let mut decoder = SseDecoder::new();
        let mut completed_response: Option<ProviderResponse> = None;

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res?;
            let events = decoder.decode(&chunk);

            for ev in events {
                if ev.data == "[DONE]" {
                    break;
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                    let ev_type = json["type"].as_str().ok_or_else(|| {
                        ProviderError::Protocol("Missing 'type' in SSE event data".into())
                    })?;
                    match ev_type {
                        "error" => {
                            let msg = json["message"].as_str().ok_or_else(|| {
                                ProviderError::Protocol("Missing 'message' in error event".into())
                            })?;
                            return Err(ProviderError::Protocol(format!("Stream error: {msg}")));
                        }
                        "response.failed" => {
                            let resp_obj = json.get("response").ok_or_else(|| {
                                ProviderError::Protocol(
                                    "Missing 'response' in response.failed event".into(),
                                )
                            })?;
                            let err_msg = resp_obj
                                .get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("Response generation failed");
                            return Err(ProviderError::Protocol(format!(
                                "Response generation failed: {err_msg}"
                            )));
                        }
                        "response.output_text.delta" => {
                            let delta = json["delta"].as_str().ok_or_else(|| {
                                ProviderError::Protocol(
                                    "Missing 'delta' in response.output_text.delta event".into(),
                                )
                            })?;
                            if let Some(tx) = &delta_sender {
                                let _ = tx.send(ProviderDelta::Text(delta.to_string())).await;
                            }
                        }
                        "response.reasoning_text.delta"
                        | "response.reasoning_summary_text.delta" => {
                            let delta = json["delta"].as_str().ok_or_else(|| {
                                ProviderError::Protocol(
                                    "Missing 'delta' in reasoning delta event".into(),
                                )
                            })?;
                            if let Some(tx) = &delta_sender {
                                let _ = tx.send(ProviderDelta::Thought(delta.to_string())).await;
                            }
                        }
                        "response.completed" => {
                            let resp_obj = json.get("response").ok_or_else(|| {
                                ProviderError::Protocol(
                                    "Missing 'response' in response.completed event".into(),
                                )
                            })?;
                            let parsed = self.parse_response(resp_obj.clone())?;
                            if let Some(tx) = &delta_sender {
                                for msg in &parsed.messages {
                                    for part in &msg.parts {
                                        if let ContentPart::Text { annotations, .. } = part {
                                            for ann in annotations {
                                                if let Annotation::UrlCitation {
                                                    title, url, ..
                                                } = ann
                                                {
                                                    let _ = tx
                                                        .send(ProviderDelta::Citation {
                                                            title: title.clone(),
                                                            url: url.clone(),
                                                        })
                                                        .await;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            completed_response = Some(parsed);
                        }
                        _ => {}
                    }
                }
            }
        }

        if let Some(ev) = decoder.flush()
            && ev.data != "[DONE]"
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data)
            && json["type"] == "response.completed"
            && let Some(resp_obj) = json.get("response")
        {
            completed_response = Some(self.parse_response(resp_obj.clone())?);
        }

        if let Some(resp) = completed_response {
            Ok(resp)
        } else {
            Err(ProviderError::Protocol(
                "Stream ended without response.completed".into(),
            ))
        }
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

        // 第一轮：无 previous_response_id，携带 reasoning_effort
        let options = CompletionOptions {
            model: "gpt-4.5-preview".into(),
            enable_web_search: true,
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        let body1 = provider.build_request_body(&session, &[], &options);
        assert!(body1.get("previous_response_id").is_none());
        assert_eq!(body1["tools"][0]["type"], "web_search");
        // Responses API 标准：reasoning.effort，禁止 top-level reasoning_effort
        assert_eq!(body1["reasoning"]["effort"], "medium");
        assert!(body1.get("reasoning_effort").is_none());

        // 模拟第一轮 Assistant 成功返回 response_id = "resp_turn_1"
        let mut asst_turn = Turn::assistant_turn(
            vec![Message::assistant(vec![ContentPart::text("It is sunny.")])],
            None,
        );
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
        assert_eq!(body2["reasoning"]["effort"], "medium");
    }

    #[test]
    fn test_response_provider_previous_id_with_tool_call_output_pair() {
        let provider = ResponseProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // 1. 用户提问
        session.append_turn(Turn::user_turn(Message::user("run dir"), None));

        // 2. Assistant 返回 ToolCall，并生成 response_id
        let mut asst_turn = Turn::assistant_turn(
            vec![Message::assistant(vec![ContentPart::ToolCall(ToolCall {
                id: "call_abc123".into(),
                name: "powershell".into(),
                arguments: r#"{"command":"dir"}"#.into(),
            })])],
            None,
        );
        asst_turn.meta.response = Some(ResponseTurnMeta::new("resp_asst_1"));
        session.append_turn(asst_turn);

        // 3. 执行工具并返回 ToolResult
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                tool_call_id: "call_abc123".into(),
                content: "dir output".into(),
                is_error: false,
            })],
            None,
        ));

        let options = CompletionOptions {
            model: "gpt-5.6-terra".into(),
            ..Default::default()
        };

        // 4. 构建下次请求 Payload：需携带 previous_response_id，且 input 必须同时包含 function_call 和 function_call_output
        let body = provider.build_request_body(&session, &[], &options);
        assert_eq!(body["previous_response_id"], "resp_asst_1");

        let inputs = body["input"].as_array().unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0]["type"], "function_call");
        assert_eq!(inputs[0]["call_id"], "call_abc123");
        assert_eq!(inputs[0]["name"], "powershell");

        assert_eq!(inputs[1]["type"], "function_call_output");
        assert_eq!(inputs[1]["call_id"], "call_abc123");
        assert_eq!(inputs[1]["output"], "dir output");
    }

    #[test]
    fn test_response_provider_parse_with_search_and_citations() {
        let provider = ResponseProvider::new("test-key");
        let mock_json = serde_json::json!({
            "id": "resp_98765",
            "status": "completed",
            "store": true,
            "reasoning": {
                "effort": "medium"
            },
            "output": [
                {
                    "type": "web_search_call",
                    "id": "call_s1",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "tokyo weather",
                        "sources": [
                            {
                                "type": "url",
                                "url": "https://weather.com/tokyo"
                            }
                        ]
                    }
                },
                {
                    "type": "message",
                    "id": "msg_search_1",
                    "role": "assistant",
                    "status": "completed",
                    "content": [
                        {
                            "type": "output_text",
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
                "total_tokens": 190,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                },
                "output_tokens_details": {
                    "reasoning_tokens": 0
                }
            }
        });

        let parsed = provider.parse_response(mock_json).expect("parse response");
        let resp_meta = parsed.meta.response.as_ref().unwrap();
        assert_eq!(resp_meta.response_id.as_deref(), Some("resp_98765"));
        assert_eq!(resp_meta.reasoning_effort.as_deref(), Some("medium"));
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
                if let Annotation::UrlCitation {
                    start_index,
                    end_index,
                    ..
                } = annotations[0]
                {
                    assert_eq!(start_index, 30);
                    assert_eq!(end_index, 33);
                } else {
                    panic!("Expected UrlCitation");
                }
            }
            _ => panic!("Expected Text with annotations"),
        }
    }

    #[test]
    fn test_response_provider_cold_start_history_and_temperature_guard() {
        let provider =
            ResponseProvider::new("test-key").with_base_url("https://api.openai.com/v1/responses/");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // System 提示
        session.append_turn(Turn::new(
            Role::System,
            vec![Message::system("Be concise.")],
            None,
        ));
        // User 问题
        session.append_turn(Turn::user_turn(Message::user("Hello"), None));
        // Assistant 回复且包含 ToolCall（来自其他 provider 或无 previous_response_id）
        let asst_msg = Message::assistant(vec![
            ContentPart::text("Calling tool"),
            ContentPart::ToolCall(ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"pwd"}"#.into(),
            }),
        ]);
        session.append_turn(Turn::assistant_turn(vec![asst_msg], None));
        // Tool 返回
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                tool_call_id: "c1".into(),
                content: "/home/project".into(),
                is_error: false,
            })],
            None,
        ));

        let options = CompletionOptions {
            model: "gpt-5.6-luna".into(),
            temperature: Some(0.7),
            reasoning_effort: Some("high".into()),
            enable_web_search: true,
            ..Default::default()
        };
        let body = provider.build_request_body(&session, &[], &options);

        // 验证推理模式下 temperature 被安全抑制
        assert!(body.get("temperature").is_none());
        assert_eq!(body["reasoning"]["effort"], "high");

        // 验证冷启动时系统消息与 Assistant 消息均被序列化进 input
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[0]["content"][0]["text"], "Be concise.");

        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["role"], "user");

        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["name"], "bash");

        assert_eq!(input[3]["type"], "message");
        assert_eq!(input[3]["role"], "assistant");
        assert_eq!(input[3]["content"][0]["type"], "output_text");

        assert_eq!(input[4]["type"], "function_call_output");
        assert_eq!(input[4]["call_id"], "c1");
    }

    #[test]
    fn test_response_provider_parse_standard_output_text_and_reasoning() {
        let provider = ResponseProvider::new("test-key");

        // 标准 OpenAI Responses API: output[].content[].type == "output_text"，以及 reasoning 块与 item_id 绑定
        let json_standard = serde_json::json!({
            "id": "resp_std_001",
            "status": "completed",
            "store": true,
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [
                        {
                            "type": "summary_text",
                            "text": "Analyzing the user query step by step"
                        }
                    ]
                },
                {
                    "type": "message",
                    "id": "msg_std_100",
                    "role": "assistant",
                    "status": "completed",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Here is the exact response from the model.",
                            "annotations": []
                        }
                    ]
                },
                {
                    "type": "function_call",
                    "id": "fc_fn_1",
                    "call_id": "call_fn_1",
                    "name": "bash",
                    "arguments": "{\"command\":\"echo hello\"}",
                    "status": "completed"
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "total_tokens": 30,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                },
                "output_tokens_details": {
                    "reasoning_tokens": 0
                }
            }
        });

        let parsed = provider
            .parse_response(json_standard)
            .expect("parse standard response");
        assert_eq!(parsed.messages.len(), 1);
        let msg = &parsed.messages[0];
        assert_eq!(
            msg.meta
                .response
                .as_ref()
                .and_then(|r| r.item_id.as_deref()),
            Some("msg_std_100")
        );
        let parts = &msg.parts;
        assert_eq!(parts.len(), 3);

        match &parts[0] {
            ContentPart::Reasoning { content, .. } => {
                assert_eq!(content, "Analyzing the user query step by step");
            }
            _ => panic!("Expected Reasoning part"),
        }

        match &parts[1] {
            ContentPart::Text { text, .. } => {
                assert_eq!(text, "Here is the exact response from the model.");
            }
            _ => panic!("Expected Text part"),
        }

        match &parts[2] {
            ContentPart::ToolCall(call) => {
                assert_eq!(call.id, "call_fn_1");
                assert_eq!(call.name, "bash");
                assert_eq!(call.arguments, "{\"command\":\"echo hello\"}");
            }
            _ => panic!("Expected ToolCall part"),
        }
    }

    #[tokio::test]
    async fn test_response_provider_stream_events_flow() {
        let provider = ResponseProvider::new("test-key");
        let (tx, mut rx) = mpsc::channel::<ProviderDelta>(10);

        let sse_stream_data = concat!(
            "event: response.reasoning_text.delta\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"Thinking deeply...\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"world!\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_stream_1\",\"status\":\"completed\",\"store\":true,\"output\":[{\"type\":\"reasoning\",\"id\":\"r1\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Thinking deeply...\"}]},{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world!\",\"annotations\":[]}]}],\"usage\":{\"input_tokens\":5,\"output_tokens\":10,\"total_tokens\":15,\"input_tokens_details\":{\"cached_tokens\":0,\"cache_write_tokens\":0},\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n\n",
            "data: [DONE]\n\n"
        );

        let mut decoder = SseDecoder::new();
        let events = decoder.decode(sse_stream_data.as_bytes());

        let mut completed_response = None;
        for ev in events {
            if ev.data == "[DONE]" {
                break;
            }
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                let ev_type = json["type"].as_str().unwrap_or_default();
                match ev_type {
                    "response.output_text.delta" => {
                        if let Some(delta) = json["delta"].as_str() {
                            let _ = tx.send(ProviderDelta::Text(delta.to_string())).await;
                        }
                    }
                    "response.reasoning_text.delta" => {
                        if let Some(delta) = json["delta"].as_str() {
                            let _ = tx.send(ProviderDelta::Thought(delta.to_string())).await;
                        }
                    }
                    "response.completed" => {
                        if let Some(resp_obj) = json.get("response") {
                            completed_response =
                                Some(provider.parse_response(resp_obj.clone()).unwrap());
                        }
                    }
                    _ => {}
                }
            }
        }

        assert_eq!(
            rx.recv().await,
            Some(ProviderDelta::Thought("Thinking deeply...".into()))
        );
        assert_eq!(rx.recv().await, Some(ProviderDelta::Text("Hello ".into())));
        assert_eq!(rx.recv().await, Some(ProviderDelta::Text("world!".into())));

        let resp = completed_response.expect("completed response");
        assert_eq!(resp.messages.len(), 1);
        assert_eq!(resp.usage.unwrap().total_tokens, 15);
    }

    #[test]
    fn test_response_provider_store_false_falls_back_to_full_history() {
        let provider = ResponseProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // 1. 用户第一轮提问
        let user_msg1 = Message::user("我需要做一般轻度 coding 开发");
        session.append_turn(Turn::user_turn(user_msg1, None));

        // 2. 模拟服务端返回 store: false 的响应报文
        let mock_unstored_json = serde_json::json!({
            "id": "resp_unstored_123",
            "status": "completed",
            "store": false,
            "previous_response_id": null,
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_ephemeral_1",
                    "summary": [],
                    "encrypted_content": "encrypted-reasoning"
                },
                {
                    "type": "web_search_call",
                    "id": "ws_ephemeral_1",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "coding model value",
                        "sources": []
                    }
                },
                {
                    "type": "message",
                    "id": "msg_ephemeral_1",
                    "role": "assistant",
                    "status": "completed",
                    "phase": "final_answer",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "建议选择轻量档位。",
                            "annotations": []
                        }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "total_tokens": 120,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                },
                "output_tokens_details": {
                    "reasoning_tokens": 0
                }
            }
        });

        let parsed = provider
            .parse_response(mock_unstored_json)
            .expect("parse unstored response");
        let resp_meta = parsed.meta.response.as_ref().unwrap();
        // 校验：当 store: false 时不记录 response_id、previous_response_id，且 store 为 false
        assert!(resp_meta.response_id.is_none());
        assert!(resp_meta.previous_response_id.is_none());
        assert_eq!(resp_meta.store, false);
        // 不记录依赖 store 的 message item_id
        assert!(parsed.messages[0].meta.response.is_none());

        let mut asst_turn = Turn::assistant_turn(parsed.messages, None);
        asst_turn.meta = parsed.meta;
        assert!(asst_turn.openai_response_id().is_none());
        session.append_turn(asst_turn);

        // 会话无法提取有效 response_id
        assert!(session.latest_openai_response_id().is_none());

        // 3. 用户发起第二轮提问
        let user_msg2 = Message::user("要考虑性价比");
        session.append_turn(Turn::user_turn(user_msg2, None));

        let options = CompletionOptions {
            model: "gpt-5.6-terra".into(),
            enable_web_search: true,
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };

        // 4. 构建第二轮请求体：因未留存，必须回退至全量发送 input，且不传 previous_response_id
        let body = provider.build_request_body(&session, &[], &options);
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["store"], true);
        assert_eq!(body["prompt_cache_key"], session.id);
        assert_eq!(
            body["include"],
            serde_json::json!([
                "reasoning.encrypted_content",
                "web_search_call.action.sources"
            ])
        );

        let input_items = body["input"].as_array().unwrap();
        // 原样回传第一轮 user、完整原生 output items 以及第二轮 user
        assert_eq!(input_items.len(), 5);
        assert_eq!(input_items[0]["role"], "user");
        assert_eq!(
            input_items[0]["content"][0]["text"],
            "我需要做一般轻度 coding 开发"
        );
        assert_eq!(input_items[1]["type"], "reasoning");
        assert_eq!(input_items[1]["encrypted_content"], "encrypted-reasoning");
        assert_eq!(input_items[2]["type"], "web_search_call");
        assert_eq!(input_items[2]["action"]["query"], "coding model value");
        assert_eq!(input_items[3]["role"], "assistant");
        assert_eq!(input_items[3]["phase"], "final_answer");
        assert_eq!(input_items[3]["content"][0]["text"], "建议选择轻量档位。");
        assert_eq!(input_items[4]["role"], "user");
        assert_eq!(input_items[4]["content"][0]["text"], "要考虑性价比");
    }

    #[test]
    fn test_response_provider_missing_store_returns_protocol_error() {
        let provider = ResponseProvider::new("test-key");
        let mock_json_no_store = serde_json::json!({
            "id": "resp_test_1",
            "status": "completed",
            "output": []
        });
        let result = provider.parse_response(mock_json_no_store);
        assert!(
            matches!(result, Err(ProviderError::Protocol(msg)) if msg.contains("Missing 'store'"))
        );
    }

    #[test]
    fn test_response_provider_failed_status_returns_protocol_error() {
        let provider = ResponseProvider::new("test-key");
        let mock_failed_json = serde_json::json!({
            "id": "resp_fail_1",
            "status": "failed",
            "store": false,
            "error": {
                "code": "server_error",
                "message": "The model failed to generate a response."
            },
            "output": []
        });
        let result = provider.parse_response(mock_failed_json);
        assert!(
            matches!(result, Err(ProviderError::Protocol(msg)) if msg.contains("Response failed: The model failed to generate a response."))
        );
    }
}
