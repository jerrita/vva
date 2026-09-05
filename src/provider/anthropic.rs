use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::common::{EventForwarder, RoleBlockMerger, require_str, require_u64, require_value};
use super::http::{SseEventHandler, post_json, post_sse};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};
use crate::event::AgentEvent;
use crate::model::{
    AnthropicTurnMeta, ContentPart, Message, Role, Session, TokenUsage, ToolCall, ToolDefinition,
    TurnMeta,
};

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com/v1".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 规范化请求端点：base_url 无论是否携带 /messages 后缀，均收敛到唯一端点
    fn normalized_endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/messages").unwrap_or(base);
        format!("{base}/messages")
    }

    /// 将 Session 转换为符合 Anthropic Messages API 的请求体（保留 thinking 签名并注入缓存断点）
    fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<serde_json::Value, ProviderError> {
        let api_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|tool| serde_json::json!({"name": tool.name, "description": tool.description, "input_schema": tool.parameters}))
            .collect();

        let mut system_text = String::new();
        // Anthropic 要求 messages 中 user 与 assistant 交替出现，相邻同角色块聚合进同一消息
        let mut merger = RoleBlockMerger::new("content");

        // 收集所有 turn 下的消息
        let mut flattened_msgs = Vec::new();
        for turn in &session.turns {
            for msg in &turn.messages {
                flattened_msgs.push((turn.role, msg));
            }
        }

        let total_msgs = flattened_msgs.len();

        for (i, (role, msg)) in flattened_msgs.iter().enumerate() {
            let is_last_user_turn = i + 1 == total_msgs && *role == Role::User;
            let should_cache = is_last_user_turn;

            match role {
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
                            },
                            _ => {}
                        }
                    }
                    merger.push("user", content_blocks);
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
                                        .map_err(|e| {
                                            ProviderError::Protocol(format!(
                                                "Invalid tool call '{}' arguments JSON: {e}",
                                                call.name
                                            ))
                                        })?;
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
                    merger.push("assistant", content_blocks);
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
                    merger.push("user", content_blocks);
                }
            }
        }

        let mut body = serde_json::json!({
            "model": options.model,
            "max_tokens": options.max_tokens.unwrap_or(4096),
            "messages": merger.into_messages(),
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

        Ok(body)
    }

    /// 解析 Anthropic Messages API 响应：非流式 DTO 与流式累积块归一到同一 `AnthropicBlock`
    /// 后经 `assemble_response` 组装（一个解析器、两个入口）
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

    fn parse_response(&self, json: serde_json::Value) -> Result<ProviderResponse, ProviderError> {
        let dto: AnthropicResponseDto = serde_json::from_value(json).map_err(|e| {
            ProviderError::Protocol(format!("Invalid Anthropic Messages API response: {e}"))
        })?;

        let blocks: Vec<AnthropicBlock> = dto.content.into_iter().map(from_dto_block).collect();
        let usage = match dto.usage {
            Some(u) => AnthropicUsageSnapshot {
                seen: true,
                input_tokens: u.input_tokens as u32,
                output_tokens: u.output_tokens as u32,
                cache_creation_input_tokens: u.cache_creation_input_tokens,
                cache_read_input_tokens: u.cache_read_input_tokens,
                stop_reason: dto.stop_reason,
            },
            None => AnthropicUsageSnapshot {
                stop_reason: dto.stop_reason,
                ..Default::default()
            },
        };

        Ok(assemble_response(blocks, usage))
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

/// Anthropic 内容块的统一累积模型：流式增量与非流式 DTO 均归一到该类型后再投影为领域 ContentPart
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
        arguments: String,
    },
    /// 安全脱敏的 thinking 块：无明文内容可投影，按官方语义跳过
    RedactedThinking,
}

impl AnthropicBlock {
    /// 投影为领域内容块；空 Text 块不产出（流式占位块与非流式空块统一处理）
    fn into_part(self) -> Option<ContentPart> {
        match self {
            AnthropicBlock::Thinking { content, signature } => {
                Some(ContentPart::Reasoning { content, signature })
            }
            AnthropicBlock::Text { text } => (!text.is_empty()).then(|| ContentPart::text(text)),
            AnthropicBlock::ToolUse {
                id,
                name,
                arguments,
            } => Some(ContentPart::ToolCall(ToolCall {
                id,
                name,
                arguments,
            })),
            AnthropicBlock::RedactedThinking => None,
        }
    }

    fn text_placeholder() -> Self {
        AnthropicBlock::Text {
            text: String::new(),
        }
    }
}

fn from_dto_block(block: AnthropicContentBlockDto) -> AnthropicBlock {
    match block {
        AnthropicContentBlockDto::Thinking {
            thinking,
            signature,
        } => AnthropicBlock::Thinking {
            content: thinking,
            signature,
        },
        AnthropicContentBlockDto::Text { text } => AnthropicBlock::Text { text },
        AnthropicContentBlockDto::ToolUse { id, name, input } => AnthropicBlock::ToolUse {
            id,
            name,
            arguments: input.to_string(),
        },
        AnthropicContentBlockDto::RedactedThinking {} => AnthropicBlock::RedactedThinking,
    }
}

/// usage / stop_reason 快照：流式逐步累积，非流式一次性提取
#[derive(Default)]
struct AnthropicUsageSnapshot {
    seen: bool,
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    stop_reason: Option<String>,
}

/// 由累积块与 usage 快照组装统一 ProviderResponse（流式与非流式共用）
fn assemble_response(
    blocks: Vec<AnthropicBlock>,
    usage: AnthropicUsageSnapshot,
) -> ProviderResponse {
    let parts: Vec<ContentPart> = blocks
        .into_iter()
        .filter_map(AnthropicBlock::into_part)
        .collect();
    let AnthropicUsageSnapshot {
        seen,
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        stop_reason,
    } = usage;
    let usage = seen.then(|| TokenUsage {
        prompt_tokens: input_tokens,
        completion_tokens: output_tokens,
        total_tokens: input_tokens + output_tokens,
        cached_tokens: cache_read_input_tokens,
        reasoning_tokens: None,
    });
    let meta = TurnMeta {
        anthropic: Some(AnthropicTurnMeta {
            cache_creation_input_tokens,
            cache_read_input_tokens,
            stop_reason,
        }),
        ..Default::default()
    };
    ProviderResponse {
        messages: vec![Message::new(parts)],
        usage,
        meta,
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
        let response = post_json(&self.normalized_endpoint(), self.auth_headers()?, &body).await?;
        self.parse_response(response)
    }

    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError> {
        let mut body = self.build_request_body(session, tools, options)?;
        body["stream"] = serde_json::json!(true);

        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(event_sender),
        };
        post_sse(
            &self.normalized_endpoint(),
            self.auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;

        Ok(assemble_response(handler.blocks, handler.usage))
    }
}

/// Anthropic 流式事件累积处理器：还原 content blocks 与 usage / stop_reason 元数据。
/// 信封字段（type / index / id 等）缺失或类型不符直接报协议错误；
/// 未知的块类型 / delta 类型因意味着内容丢失，同样显式报错而非静默吞没。
struct AnthropicStreamHandler {
    blocks: Vec<AnthropicBlock>,
    usage: AnthropicUsageSnapshot,
    forwarder: EventForwarder,
}

impl AnthropicStreamHandler {
    /// 保证 blocks 至少有 `len` 个槽位，不足处以空 Text 块占位
    fn ensure_block_slots(&mut self, len: usize) {
        while self.blocks.len() < len {
            self.blocks.push(AnthropicBlock::text_placeholder());
        }
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
                    "thinking" => AnthropicBlock::Thinking {
                        content: String::new(),
                        signature: None,
                    },
                    "text" => AnthropicBlock::Text {
                        // 官方规范中起始 text 恒为空串，允许省略
                        text: cb["text"].as_str().unwrap_or_default().to_string(),
                    },
                    "tool_use" => AnthropicBlock::ToolUse {
                        id: require_str(cb, "id")?.to_string(),
                        name: require_str(cb, "name")?.to_string(),
                        arguments: String::new(),
                    },
                    "redacted_thinking" => AnthropicBlock::RedactedThinking,
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
                            Some(AnthropicBlock::Thinking { content, .. }) => content.push_str(t),
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
                            Some(AnthropicBlock::Text { text }) => text.push_str(t),
                            _ => {
                                return Err(ProviderError::Protocol(format!(
                                    "text_delta on non-text block {idx}"
                                )));
                            }
                        }
                        self.forwarder.text(t).await;
                    }
                    "signature_delta" => {
                        let sig = require_str(delta, "signature")?;
                        match self.blocks.get_mut(idx) {
                            Some(AnthropicBlock::Thinking { signature, .. }) => {
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
                            Some(AnthropicBlock::ToolUse { arguments, .. }) => {
                                arguments.push_str(pj);
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
        let user_msg = Message::text("Solve this puzzle with deep thinking");
        session.append_turn(Turn::user_turn(user_msg));

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
        let mut assistant_turn = Turn::assistant_turn(parsed.messages);
        assistant_turn.meta = parsed.meta;
        session.append_turn(assistant_turn);

        // 下一轮用户继续追问
        session.append_turn(Turn::user_turn(Message::text("Why 42?")));

        // 生成下一次调用的 request body，验证 thinking 和 signature 被完整保留
        let options = CompletionOptions {
            model: "claude-3-7-sonnet-20250219".into(),
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");
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

        session.append_turn(Turn::user_turn(Message::text("List files and run command")));

        let asst_msg = Message::new(vec![
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
        session.append_turn(Turn::assistant_turn(vec![asst_msg]));

        // 两个连续的 Tool turn
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                tool_call_id: "call_1".into(),
                content: "file contents".into(),
                is_error: false,
            })],
        ));
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                tool_call_id: "call_2".into(),
                content: "command output".into(),
                is_error: false,
            })],
        ));

        let options = CompletionOptions {
            model: "claude-3-7-sonnet".into(),
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");
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

    /// 流式增量累积与非流式解析必须产出同一结果（一个解析器、两个入口）
    #[tokio::test]
    async fn test_anthropic_streaming_matches_non_streaming_parse() {
        let provider = AnthropicProvider::new("test-key");
        let full = serde_json::json!({
            "id": "msg_stream_1",
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "Step 1.", "signature": "sig_abc" },
                { "type": "text", "text": "Answer body." },
                { "type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"command": "ls"} }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 40,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 80
            }
        });

        let parsed = provider.parse_response(full.clone()).expect("parse full");

        // 用等价的流式事件序列驱动同一个组装路径
        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(None),
        };
        let events = [
            serde_json::json!({
                "type": "message_start",
                "message": {
                    "usage": {
                        "input_tokens": 100,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 80
                    }
                }
            }),
            serde_json::json!({
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "thinking", "thinking": "" }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": { "type": "thinking_delta", "thinking": "Step 1." }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": { "type": "signature_delta", "signature": "sig_abc" }
            }),
            serde_json::json!({
                "type": "content_block_start", "index": 1,
                "content_block": { "type": "text", "text": "" }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 1,
                "delta": { "type": "text_delta", "text": "Answer body." }
            }),
            serde_json::json!({
                "type": "content_block_start", "index": 2,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": "bash", "input": {} }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 2,
                "delta": { "type": "input_json_delta", "partial_json": "{\"command\":\"ls\"}" }
            }),
            serde_json::json!({
                "type": "content_block_stop", "index": 2
            }),
            serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "tool_use" },
                "usage": { "output_tokens": 40 }
            }),
            serde_json::json!({ "type": "message_stop" }),
        ];
        for ev in events {
            handler.on_event(ev).await.expect("handle stream event");
        }
        let streamed = assemble_response(handler.blocks, handler.usage);

        assert_eq!(streamed.messages[0].parts, parsed.messages[0].parts);
        assert_eq!(streamed.usage, parsed.usage);
        assert_eq!(streamed.meta.anthropic, parsed.meta.anthropic);
    }

    #[tokio::test]
    async fn test_anthropic_stream_error_event_reports_protocol_error() {
        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(None),
        };
        let err = handler
            .on_event(serde_json::json!({
                "type": "error",
                "error": { "type": "overloaded_error", "message": "Overloaded" }
            }))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Protocol(ref msg) if msg.contains("Overloaded")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_anthropic_stream_unknown_block_type_reports_protocol_error() {
        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(None),
        };
        let err = handler
            .on_event(serde_json::json!({
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "server_tool_use", "id": "x", "name": "y" }
            }))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Protocol(ref msg) if msg.contains("server_tool_use")),
            "unexpected error: {err:?}"
        );
    }
}
