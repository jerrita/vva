use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::{
    CompletionOptions, Provider, ProviderCapabilities, ProviderDelta, ProviderError,
    ProviderResponse, sse::SseDecoder,
};
use crate::model::{
    AnthropicTurnMeta, ContentPart, Message, Role, Session, TokenUsage, ToolCall, ToolDefinition,
    TurnMeta,
};

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com/v1".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 将 Session 转换为符合 Anthropic Messages API 的请求体（保留 thinking 签名并注入缓存断点）
    pub fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> serde_json::Value {
        let mut api_tools: Vec<serde_json::Value> = Vec::new();
        for tool in tools {
            api_tools.push(serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            }));
        }

        let mut system_text = String::new();
        let mut messages_payload: Vec<serde_json::Value> = Vec::new();

        // 收集所有 turn 下的消息
        let mut flattened_msgs = Vec::new();
        for turn in &session.turns {
            for msg in &turn.messages {
                flattened_msgs.push(msg);
            }
        }

        let total_msgs = flattened_msgs.len();
        // Anthropic 要求 messages 中 user 与 assistant 交替出现
        let mut append_blocks = |role: &str, blocks: Vec<serde_json::Value>| {
            if blocks.is_empty() {
                return;
            }
            if let Some(last) = messages_payload.last_mut()
                && last["role"] == role
                && let Some(arr) = last["content"].as_array_mut()
            {
                for b in blocks {
                    arr.push(b);
                }
                return;
            }
            messages_payload.push(serde_json::json!({
                "role": role,
                "content": blocks,
            }));
        };

        for (i, msg) in flattened_msgs.iter().enumerate() {
            let is_last_user_turn = i + 1 == total_msgs && msg.role == Role::User;
            let should_cache = msg
                .meta
                .anthropic
                .as_ref()
                .map(|a| a.ephemeral_cache)
                .unwrap_or(false)
                || is_last_user_turn;

            match msg.role {
                Role::System => {
                    system_text.push_str(&msg.text_content());
                }
                Role::User => {
                    let mut content_blocks = Vec::new();
                    for part in &msg.parts {
                        match part {
                            ContentPart::Text { text, .. } => {
                                let mut block = serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                });
                                if should_cache {
                                    block["cache_control"] =
                                        serde_json::json!({ "type": "ephemeral" });
                                }
                                content_blocks.push(block);
                            }
                            ContentPart::Image { source, .. } => match source {
                                crate::model::MediaSource::Base64 { data, mime_type } => {
                                    content_blocks.push(serde_json::json!({
                                        "type": "image",
                                        "source": {
                                            "type": "base64",
                                            "media_type": mime_type,
                                            "data": data,
                                        }
                                    }));
                                }
                                crate::model::MediaSource::Url { url } => {
                                    content_blocks.push(serde_json::json!({
                                        "type": "image",
                                        "source": {
                                            "type": "url",
                                            "url": url,
                                        }
                                    }));
                                }
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                    append_blocks("user", content_blocks);
                }
                Role::Assistant => {
                    let mut content_blocks = Vec::new();
                    for part in &msg.parts {
                        match part {
                            ContentPart::Reasoning { content, signature } => {
                                // 核心原生优势：将带数字签名的 thinking 块原样带回，防止被 API 拒绝
                                let mut block = serde_json::json!({
                                    "type": "thinking",
                                    "thinking": content,
                                });
                                if let Some(sig) = signature {
                                    block["signature"] = serde_json::json!(sig);
                                }
                                content_blocks.push(block);
                            }
                            ContentPart::Text { text, .. } => {
                                content_blocks.push(serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                }));
                            }
                            ContentPart::ToolCall(call) => {
                                let args_val =
                                    serde_json::from_str::<serde_json::Value>(&call.arguments)
                                        .unwrap_or_else(|_| serde_json::json!({}));
                                content_blocks.push(serde_json::json!({
                                    "type": "tool_use",
                                    "id": call.id,
                                    "name": call.name,
                                    "input": args_val,
                                }));
                            }
                            _ => {}
                        }
                    }
                    append_blocks("assistant", content_blocks);
                }
                Role::Tool => {
                    let mut content_blocks = Vec::new();
                    for part in &msg.parts {
                        if let ContentPart::ToolResult(res) = part {
                            let mut block = serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": res.tool_call_id,
                                "content": res.content,
                                "is_error": res.is_error,
                            });
                            if should_cache {
                                block["cache_control"] = serde_json::json!({ "type": "ephemeral" });
                            }
                            content_blocks.push(block);
                        }
                    }
                    append_blocks("user", content_blocks);
                }
            }
        }

        let mut body = serde_json::json!({
            "model": options.model,
            "max_tokens": options.max_tokens.unwrap_or(4096),
            "messages": messages_payload,
        });

        if !system_text.is_empty() {
            body["system"] = serde_json::json!([
                {
                    "type": "text",
                    "text": system_text,
                    "cache_control": { "type": "ephemeral" } // 系统指令默认启用 Prompt Caching
                }
            ]);
        }

        if !api_tools.is_empty() {
            body["tools"] = serde_json::Value::Array(api_tools);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        body
    }

    /// 解析 Anthropic Messages API 响应
    pub fn parse_response(
        &self,
        json: serde_json::Value,
    ) -> Result<ProviderResponse, ProviderError> {
        let mut parts = Vec::new();

        if let Some(contents) = json["content"].as_array() {
            for c in contents {
                let block_type = c["type"].as_str().unwrap_or_default();
                match block_type {
                    "thinking" => {
                        let content = c["thinking"].as_str().unwrap_or_default().to_string();
                        let signature = c["signature"].as_str().map(|s| s.to_string());
                        parts.push(ContentPart::Reasoning { content, signature });
                    }
                    "text" => {
                        let text = c["text"].as_str().unwrap_or_default().to_string();
                        parts.push(ContentPart::text(text));
                    }
                    "tool_use" => {
                        let id = c["id"].as_str().unwrap_or_default().to_string();
                        let name = c["name"].as_str().unwrap_or_default().to_string();
                        let arguments = c["input"].to_string();
                        parts.push(ContentPart::ToolCall(ToolCall {
                            id,
                            name,
                            arguments,
                        }));
                    }
                    _ => {}
                }
            }
        }

        let mut usage = None;
        let mut turn_meta = TurnMeta::default();

        if let Some(u) = json.get("usage") {
            let prompt_tokens = u["input_tokens"].as_u64().unwrap_or(0) as u32;
            let completion_tokens = u["output_tokens"].as_u64().unwrap_or(0) as u32;
            let cache_creation = u["cache_creation_input_tokens"].as_u64().map(|v| v as u32);
            let cache_read = u["cache_read_input_tokens"].as_u64().map(|v| v as u32);

            usage = Some(TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
                cached_tokens: cache_read,
                reasoning_tokens: None,
            });

            turn_meta.anthropic = Some(AnthropicTurnMeta {
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
                stop_reason: json["stop_reason"].as_str().map(|s| s.to_string()),
            });
        }

        let message = Message::assistant(parts);

        Ok(ProviderResponse {
            messages: vec![message],
            usage,
            meta: turn_meta,
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_native_search: false,
            supports_prompt_caching: true,
            supports_thinking_signature: true,
            supports_stateful_response_id: false,
        }
    }

    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        let base = self.base_url.trim_end_matches('/');
        let url = if base.ends_with("/messages") {
            base.to_string()
        } else {
            format!("{base}/messages")
        };
        let body = self.build_request_body(session, tools, options);

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
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
        let url = if base.ends_with("/messages") {
            base.to_string()
        } else {
            format!("{base}/messages")
        };
        let mut body = self.build_request_body(session, tools, options);
        body["stream"] = serde_json::json!(true);

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
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

        enum AnthropicBlock {
            Thinking {
                content: String,
                signature: Option<String>,
            },
            Text {
                text: String,
            },
            ToolUse {
                id: String,
                name: String,
                arguments_buf: String,
            },
        }

        let mut blocks: Vec<AnthropicBlock> = Vec::new();
        let mut prompt_tokens: u32 = 0;
        let mut completion_tokens: u32 = 0;
        let mut cache_creation: Option<u32> = None;
        let mut cache_read: Option<u32> = None;
        let mut stop_reason: Option<String> = None;

        let mut process_event =
            |json: &serde_json::Value, tx_opt: &Option<mpsc::Sender<ProviderDelta>>| {
                let ev_type = json["type"].as_str().unwrap_or_default();
                match ev_type {
                    "message_start" => {
                        if let Some(u) = json.get("message").and_then(|m| m.get("usage")) {
                            prompt_tokens = u["input_tokens"].as_u64().unwrap_or(0) as u32;
                            cache_creation =
                                u["cache_creation_input_tokens"].as_u64().map(|v| v as u32);
                            cache_read = u["cache_read_input_tokens"].as_u64().map(|v| v as u32);
                        }
                    }
                    "content_block_start" => {
                        let idx = json["index"].as_u64().unwrap_or(0) as usize;
                        let cb = &json["content_block"];
                        let block_type = cb["type"].as_str().unwrap_or_default();
                        let block = match block_type {
                            "thinking" => AnthropicBlock::Thinking {
                                content: String::new(),
                                signature: None,
                            },
                            "text" => AnthropicBlock::Text {
                                text: cb["text"].as_str().unwrap_or_default().to_string(),
                            },
                            "tool_use" => AnthropicBlock::ToolUse {
                                id: cb["id"].as_str().unwrap_or_default().to_string(),
                                name: cb["name"].as_str().unwrap_or_default().to_string(),
                                arguments_buf: String::new(),
                            },
                            _ => AnthropicBlock::Text {
                                text: String::new(),
                            },
                        };
                        while blocks.len() <= idx {
                            blocks.push(AnthropicBlock::Text {
                                text: String::new(),
                            });
                        }
                        blocks[idx] = block;
                    }
                    "content_block_delta" => {
                        let idx = json["index"].as_u64().unwrap_or(0) as usize;
                        let delta = &json["delta"];
                        let delta_type = delta["type"].as_str().unwrap_or_default();
                        match delta_type {
                            "thinking_delta" => {
                                if let Some(t) = delta["thinking"].as_str() {
                                    if let Some(AnthropicBlock::Thinking { content, .. }) =
                                        blocks.get_mut(idx)
                                    {
                                        content.push_str(t);
                                    }
                                    if let Some(tx) = tx_opt {
                                        let _ = tx.try_send(ProviderDelta::Thought(t.to_string()));
                                    }
                                }
                            }
                            "text_delta" => {
                                if let Some(t) = delta["text"].as_str() {
                                    if let Some(AnthropicBlock::Text { text }) = blocks.get_mut(idx)
                                    {
                                        text.push_str(t);
                                    }
                                    if let Some(tx) = tx_opt {
                                        let _ = tx.try_send(ProviderDelta::Text(t.to_string()));
                                    }
                                }
                            }
                            "signature_delta" => {
                                if let Some(sig) = delta["signature"].as_str()
                                    && let Some(AnthropicBlock::Thinking { signature, .. }) =
                                        blocks.get_mut(idx)
                                {
                                    *signature = Some(sig.to_string());
                                }
                            }
                            "input_json_delta" => {
                                if let Some(pj) = delta["partial_json"].as_str()
                                    && let Some(AnthropicBlock::ToolUse { arguments_buf, .. }) =
                                        blocks.get_mut(idx)
                                {
                                    arguments_buf.push_str(pj);
                                }
                            }
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(sr) = json["delta"]["stop_reason"].as_str() {
                            stop_reason = Some(sr.to_string());
                        }
                        if let Some(u) = json.get("usage")
                            && let Some(out) = u["output_tokens"].as_u64()
                        {
                            completion_tokens = out as u32;
                        }
                    }
                    _ => {}
                }
            };

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res?;
            let events = decoder.decode(&chunk);

            for ev in events {
                if ev.data == "[DONE]" {
                    break;
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                    process_event(&json, &delta_sender);
                }
            }
        }

        if let Some(ev) = decoder.flush()
            && ev.data != "[DONE]"
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data)
        {
            process_event(&json, &delta_sender);
        }

        let mut parts = Vec::new();
        for b in blocks {
            match b {
                AnthropicBlock::Thinking { content, signature } => {
                    parts.push(ContentPart::Reasoning { content, signature });
                }
                AnthropicBlock::Text { text } => {
                    if !text.is_empty() {
                        parts.push(ContentPart::text(text));
                    }
                }
                AnthropicBlock::ToolUse {
                    id,
                    name,
                    arguments_buf,
                } => {
                    parts.push(ContentPart::ToolCall(ToolCall {
                        id,
                        name,
                        arguments: arguments_buf,
                    }));
                }
            }
        }

        let usage = Some(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cached_tokens: cache_read,
            reasoning_tokens: None,
        });

        let turn_meta = TurnMeta {
            anthropic: Some(AnthropicTurnMeta {
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
                stop_reason,
            }),
            ..Default::default()
        };

        let message = Message::assistant(parts);

        Ok(ProviderResponse {
            messages: vec![message],
            usage,
            meta: turn_meta,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::path::PathBuf;

    #[test]
    fn test_anthropic_thinking_signature_and_caching_roundtrip() {
        let provider = AnthropicProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // 用户输入
        let user_msg = Message::user("Solve this puzzle with deep thinking");
        session.append_turn(Turn::user_turn(user_msg, None));

        // 模拟 Anthropic 返回包含 thinking block 和 signature 的响应
        let mock_resp = serde_json::json!({
            "id": "msg_01X999",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "thinking",
                    "thinking": "Step 1: analyze conditions. Step 2: deduce solution.",
                    "signature": "sig_cryptographic_verification_token"
                },
                {
                    "type": "text",
                    "text": "The answer is 42."
                }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 1200,
                "output_tokens": 350,
                "cache_creation_input_tokens": 1000,
                "cache_read_input_tokens": 0
            }
        });

        let parsed = provider.parse_response(mock_resp).expect("parse response");
        let anthropic_meta = parsed.meta.anthropic.as_ref().unwrap();
        assert_eq!(anthropic_meta.cache_creation_input_tokens, Some(1000));
        assert_eq!(anthropic_meta.stop_reason.as_deref(), Some("end_turn"));

        // 将 assistant turn 加入 session
        let mut assistant_turn = Turn::assistant_turn(parsed.messages, None);
        assistant_turn.meta = parsed.meta;
        session.append_turn(assistant_turn);

        // 下一轮用户继续追问
        session.append_turn(Turn::user_turn(Message::user("Why 42?"), None));

        // 生成下一次调用的 request body，验证 thinking 和 signature 被完整保留
        let options = CompletionOptions {
            model: "claude-3-7-sonnet-20250219".into(),
            ..Default::default()
        };
        let body = provider.build_request_body(&session, &[], &options);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);

        // 检查第二条（assistant 历史）中是否保留了 signature
        let asst_content = msgs[1]["content"].as_array().unwrap();
        assert_eq!(asst_content[0]["type"], "thinking");
        assert_eq!(
            asst_content[0]["signature"],
            "sig_cryptographic_verification_token"
        );

        // 检查最后一条用户消息是否自动注入了 cache_control
        let last_user_content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(last_user_content[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_anthropic_tool_results_merged_into_single_user_turn() {
        let provider = AnthropicProvider::new("test-key")
            .with_base_url("https://proxy.example.com/v1/messages/");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        session.append_turn(Turn::user_turn(
            Message::user("List files and run command"),
            None,
        ));

        let asst_msg = Message::assistant(vec![
            ContentPart::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "file_view".into(),
                arguments: "{}".into(),
            }),
            ContentPart::ToolCall(ToolCall {
                id: "call_2".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }),
        ]);
        session.append_turn(Turn::assistant_turn(vec![asst_msg], None));

        // 两个连续的 Tool turn
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                tool_call_id: "call_1".into(),
                content: "file contents".into(),
                is_error: false,
            })],
            None,
        ));
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                tool_call_id: "call_2".into(),
                content: "command output".into(),
                is_error: false,
            })],
            None,
        ));

        let options = CompletionOptions {
            model: "claude-3-7-sonnet".into(),
            ..Default::default()
        };
        let body = provider.build_request_body(&session, &[], &options);
        let msgs = body["messages"].as_array().unwrap();

        // 验证两个 tool result 被合并到了同一个 user message 中，保证严格交替：user -> assistant -> user
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[2]["role"], "user");
        let tool_blocks = msgs[2]["content"].as_array().unwrap();
        assert_eq!(tool_blocks.len(), 2);
        assert_eq!(tool_blocks[0]["type"], "tool_result");
        assert_eq!(tool_blocks[1]["type"], "tool_result");
    }

    #[tokio::test]
    async fn test_anthropic_provider_stream_events_flow() {
        let (tx, mut rx) = mpsc::channel::<ProviderDelta>(10);

        let sse_data = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":25,\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":5}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Reasoning about code...\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_xyz\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Here is the solution.\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":40}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let mut decoder = SseDecoder::new();
        let events = decoder.decode(sse_data.as_bytes());

        for ev in events {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data)
                && json["type"] == "content_block_delta"
            {
                let delta = &json["delta"];
                if delta["type"] == "thinking_delta" {
                    let _ = tx
                        .send(ProviderDelta::Thought(
                            delta["thinking"].as_str().unwrap().to_string(),
                        ))
                        .await;
                } else if delta["type"] == "text_delta" {
                    let _ = tx
                        .send(ProviderDelta::Text(
                            delta["text"].as_str().unwrap().to_string(),
                        ))
                        .await;
                }
            }
        }

        assert_eq!(
            rx.recv().await,
            Some(ProviderDelta::Thought("Reasoning about code...".into()))
        );
        assert_eq!(
            rx.recv().await,
            Some(ProviderDelta::Text("Here is the solution.".into()))
        );
    }
}
