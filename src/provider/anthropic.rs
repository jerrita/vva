use async_trait::async_trait;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::http::{SseEventHandler, consume_sse, post_json, post_sse};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};
use crate::event::AgentEvent;
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
            client: super::shared_client().clone(),
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com/v1".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 规范化请求端点：base_url 无论是否携带 /messages 后缀，均收敛到唯一端点
    fn endpoint_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/messages").unwrap_or(base);
        format!("{base}/messages")
    }

    /// x-api-key 鉴权、协议版本与 JSON Content-Type 头
    fn auth_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
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
        let dto: AnthropicResponseDto = serde_json::from_value(json).map_err(|e| {
            ProviderError::Protocol(format!("Invalid Anthropic Messages API response: {e}"))
        })?;

        let mut parts = Vec::new();
        for block in dto.content {
            match block {
                AnthropicContentBlockDto::Thinking { thinking, signature } => {
                    parts.push(ContentPart::Reasoning {
                        content: thinking,
                        signature,
                    });
                }
                AnthropicContentBlockDto::Text { text } => {
                    parts.push(ContentPart::text(text));
                }
                AnthropicContentBlockDto::ToolUse { id, name, input } => {
                    parts.push(ContentPart::ToolCall(ToolCall {
                        id,
                        name,
                        arguments: input.to_string(),
                    }));
                }
                // 安全脱敏的 thinking 块：无明文内容可投影，按官方语义跳过
                AnthropicContentBlockDto::RedactedThinking {} => {}
            }
        }

        let mut usage = None;
        let mut turn_meta = TurnMeta::default();

        if let Some(u) = dto.usage {
            let cache_creation = u.cache_creation_input_tokens;
            let cache_read = u.cache_read_input_tokens;

            usage = Some(TokenUsage {
                prompt_tokens: u.input_tokens as u32,
                completion_tokens: u.output_tokens as u32,
                total_tokens: (u.input_tokens + u.output_tokens) as u32,
                cached_tokens: cache_read,
                reasoning_tokens: None,
            });

            turn_meta.anthropic = Some(AnthropicTurnMeta {
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
                stop_reason: dto.stop_reason,
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

/// Anthropic Messages API 响应报文（serde 严格校验，字段缺失即协议错误）
#[derive(serde::Deserialize)]
struct AnthropicResponseDto {
    content: Vec<AnthropicContentBlockDto>,
    #[serde(default)]
    usage: Option<AnthropicUsageDto>,
    #[serde(default)]
    stop_reason: Option<String>,
}

/// Anthropic Messages API 内容块
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlockDto {
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    RedactedThinking {},
}

/// Anthropic Messages API usage 统计
#[derive(serde::Deserialize)]
struct AnthropicUsageDto {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
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
        let body = self.build_request_body(session, tools, options);
        let json_resp = post_json(&self.client, &self.endpoint_url(), self.auth_headers()?, &body)
            .await?;
        self.parse_response(json_resp)
    }

    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError> {
        let mut body = self.build_request_body(session, tools, options);
        body["stream"] = serde_json::json!(true);

        let mut headers = self.auth_headers()?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        let res = post_sse(&self.client, &self.endpoint_url(), headers, &body).await?;

        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cache_creation: None,
            cache_read: None,
            stop_reason: None,
            sender: event_sender,
        };
        consume_sse(res, &mut handler).await?;

        let AnthropicStreamHandler {
            blocks,
            prompt_tokens,
            completion_tokens,
            cache_creation,
            cache_read,
            stop_reason,
            ..
        } = handler;

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

/// Anthropic Messages API 流式内容块的累积模型
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

/// Anthropic 流式事件累积处理器：还原 content blocks 与 usage / stop_reason 元数据
struct AnthropicStreamHandler {
    blocks: Vec<AnthropicBlock>,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_creation: Option<u32>,
    cache_read: Option<u32>,
    stop_reason: Option<String>,
    sender: Option<mpsc::Sender<AgentEvent>>,
}

#[async_trait]
impl SseEventHandler for AnthropicStreamHandler {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError> {
        let ev_type = json["type"].as_str().unwrap_or_default();
        match ev_type {
            "message_start" => {
                if let Some(u) = json.get("message").and_then(|m| m.get("usage")) {
                    self.prompt_tokens = u["input_tokens"].as_u64().unwrap_or(0) as u32;
                    self.cache_creation =
                        u["cache_creation_input_tokens"].as_u64().map(|v| v as u32);
                    self.cache_read = u["cache_read_input_tokens"].as_u64().map(|v| v as u32);
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
                while self.blocks.len() <= idx {
                    self.blocks.push(AnthropicBlock::Text {
                        text: String::new(),
                    });
                }
                self.blocks[idx] = block;
            }
            "content_block_delta" => {
                let idx = json["index"].as_u64().unwrap_or(0) as usize;
                let delta = &json["delta"];
                let delta_type = delta["type"].as_str().unwrap_or_default();
                match delta_type {
                    "thinking_delta" => {
                        if let Some(t) = delta["thinking"].as_str() {
                            if let Some(AnthropicBlock::Thinking { content, .. }) =
                                self.blocks.get_mut(idx)
                            {
                                content.push_str(t);
                            }
                            if let Some(tx) = &self.sender {
                                let _ = tx.try_send(AgentEvent::Thought(t.to_string()));
                            }
                        }
                    }
                    "text_delta" => {
                        if let Some(t) = delta["text"].as_str() {
                            if let Some(AnthropicBlock::Text { text }) = self.blocks.get_mut(idx) {
                                text.push_str(t);
                            }
                            if let Some(tx) = &self.sender {
                                let _ = tx.try_send(AgentEvent::Text(t.to_string()));
                            }
                        }
                    }
                    "signature_delta" => {
                        if let Some(sig) = delta["signature"].as_str()
                            && let Some(AnthropicBlock::Thinking { signature, .. }) =
                                self.blocks.get_mut(idx)
                        {
                            *signature = Some(sig.to_string());
                        }
                    }
                    "input_json_delta" => {
                        if let Some(pj) = delta["partial_json"].as_str()
                            && let Some(AnthropicBlock::ToolUse { arguments_buf, .. }) =
                                self.blocks.get_mut(idx)
                        {
                            arguments_buf.push_str(pj);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(sr) = json["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(sr.to_string());
                }
                if let Some(u) = json.get("usage")
                    && let Some(out) = u["output_tokens"].as_u64()
                {
                    self.completion_tokens = out as u32;
                }
            }
            _ => {}
        }
        Ok(())
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
}

