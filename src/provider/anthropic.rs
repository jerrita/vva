use std::collections::HashMap;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::common::{EventForwarder, RoleBlockMerger, require_str, require_u64, require_value};
use super::http::{SseEventHandler, post_json, post_sse};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};
use crate::event::AgentEvent;
use crate::model::{
    Annotation, AnthropicCitation, AnthropicContentBlock, AnthropicFetchResultContent,
    AnthropicSearchResult, AnthropicSearchResultContent, AnthropicTurnMeta, ContentPart, Message,
    Role, SearchStatus, Session, TokenUsage, ToolCall, ToolDefinition, TurnMeta, WebSearchAction,
    WebSearchCall, WebSearchSource,
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
        let mut api_tools: Vec<serde_json::Value> = Vec::new();
        // 核心原生优势：服务端代执行的 web_search / web_fetch（引用与结果块由服务端直接产出）
        if options.enable_web_search {
            // _20260209 起的版本默认经代码执行容器调用；模型无 programmatic tool
            // calling 能力时必须显式声明直连，否则 API 返回 400
            let direct_callers = serde_json::json!(["direct"]);
            api_tools.push(serde_json::json!({
                "type": "web_search_20260318",
                "name": "web_search",
                "allowed_callers": direct_callers,
            }));
            // 模型级 annotation 可关闭 web_fetch（未接线 web_fetch 的中转端点会整组丢弃工具声明）
            if options.enable_web_fetch {
                api_tools.push(serde_json::json!({
                    "type": "web_fetch_20260318",
                    "name": "web_fetch",
                    "allowed_callers": direct_callers,
                }));
            }
        }
        api_tools.extend(tools.iter().map(|tool| {
            serde_json::json!({"name": tool.name, "description": tool.description, "input_schema": tool.parameters})
        }));

        let mut system_text = String::new();
        // Anthropic 要求 messages 中 user 与 assistant 交替出现，相邻同角色块聚合进同一消息
        let mut merger = RoleBlockMerger::new("content");

        // 收集所有 turn 下的消息
        let mut flattened_msgs: Vec<(Role, &Message, &TurnMeta)> = Vec::new();
        for turn in &session.turns {
            for msg in &turn.messages {
                flattened_msgs.push((turn.role, msg, &turn.meta));
            }
        }

        let total_msgs = flattened_msgs.len();

        for (i, (role, msg, meta)) in flattened_msgs.iter().enumerate() {
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
                            ContentPart::File {
                                source: crate::model::MediaSource::Base64 { data, mime_type },
                                ..
                            } => {
                                content_blocks.push(serde_json::json!({
                                    "type": "document",
                                    "source": {
                                        "type": "base64",
                                        "media_type": mime_type,
                                        "data": data,
                                    }
                                }));
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
                    // 核心原生优势：原生块链保真回传（encrypted_content / encrypted_index
                    // 不可重建，逐块原样带回），同 Responses output_items 策略
                    if let Some(anthropic) = meta.anthropic.as_ref()
                        && !anthropic.content_blocks.is_empty()
                    {
                        let native_blocks = anthropic
                            .content_blocks
                            .iter()
                            .map(serde_json::to_value)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|e| {
                                ProviderError::Protocol(format!("Serialize native blocks: {e}"))
                            })?;
                        merger.push("assistant", native_blocks);
                        continue;
                    }
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
                                "content": tool_output(res)?,
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

    /// 解析 Anthropic Messages API 响应：content 直接反序列化为原生块链，
    /// 经 `assemble_response` 组装（流式与非流式共用同一组装路径）
    fn parse_response(&self, json: serde_json::Value) -> Result<ProviderResponse, ProviderError> {
        let dto: AnthropicResponseDto = serde_json::from_value(json).map_err(|e| {
            ProviderError::Protocol(format!("Invalid Anthropic Messages API response: {e}"))
        })?;

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

        assemble_response(dto.content, usage)
    }

    /// x-api-key 鉴权与 JSON Content-Type 头
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
}

/// Anthropic Messages API 响应报文（serde 严格校验，字段缺失即协议错误）
#[derive(serde::Deserialize)]
struct AnthropicResponseDto {
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsageDto>,
    #[serde(default)]
    stop_reason: Option<String>,
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

/// 将原生块序列投影为领域内容块：server tool 调用与其结果块配对为一条 WebSearchCall，
/// 引用注脚注入对应文本块；加密协议字段仅保留在 TurnMeta 原生块链中
fn project_parts(blocks: &[AnthropicContentBlock]) -> Result<Vec<ContentPart>, ProviderError> {
    let mut parts = Vec::new();
    // 待配对的服务端调用：id -> (name, input)，结果块按 tool_use_id 取回
    let mut server_uses: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    for block in blocks {
        match block {
            AnthropicContentBlock::Thinking {
                thinking,
                signature,
            } => parts.push(ContentPart::Reasoning {
                content: thinking.clone(),
                signature: signature.clone(),
            }),
            AnthropicContentBlock::Text { text, citations } => {
                if !text.is_empty() {
                    parts.push(ContentPart::Text {
                        text: text.clone(),
                        annotations: citations.iter().map(project_annotation).collect(),
                    });
                }
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                parts.push(ContentPart::ToolCall(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: input.to_string(),
                }));
            }
            AnthropicContentBlock::ServerToolUse { id, name, input } => {
                server_uses.insert(id.clone(), (name.clone(), input.clone()));
            }
            AnthropicContentBlock::WebSearchToolResult {
                tool_use_id,
                content,
            } => {
                let (name, input) = take_server_use(&mut server_uses, tool_use_id)?;
                if name != "web_search" {
                    return Err(ProviderError::Protocol(format!(
                        "web_search_tool_result paired with server_tool_use '{name}'"
                    )));
                }
                parts.push(ContentPart::WebSearchCall(project_search_call(
                    tool_use_id,
                    &input,
                    content,
                )?));
            }
            AnthropicContentBlock::WebFetchToolResult {
                tool_use_id,
                content,
            } => {
                let (name, input) = take_server_use(&mut server_uses, tool_use_id)?;
                if name != "web_fetch" {
                    return Err(ProviderError::Protocol(format!(
                        "web_fetch_tool_result paired with server_tool_use '{name}'"
                    )));
                }
                parts.push(ContentPart::WebSearchCall(project_fetch_call(
                    tool_use_id,
                    &input,
                    content,
                )?));
            }
            // 安全脱敏的 thinking 块：无明文内容可投影，按官方语义跳过
            AnthropicContentBlock::RedactedThinking { .. } => {}
        }
    }
    Ok(parts)
}

fn take_server_use(
    server_uses: &mut HashMap<String, (String, serde_json::Value)>,
    tool_use_id: &str,
) -> Result<(String, serde_json::Value), ProviderError> {
    server_uses.remove(tool_use_id).ok_or_else(|| {
        ProviderError::Protocol(format!(
            "server tool result without preceding server_tool_use '{tool_use_id}'"
        ))
    })
}

fn project_annotation(citation: &AnthropicCitation) -> Annotation {
    let AnthropicCitation::WebSearchResultLocation {
        url,
        title,
        cited_text,
        ..
    } = citation;
    Annotation::WebSearchResultLocation {
        url: url.clone(),
        title: title.clone(),
        cited_text: cited_text.clone(),
    }
}

/// web_search 调用对投影为 Search 动作：结果列表转来源，错误对象按 Failed 上报
fn project_search_call(
    id: &str,
    input: &serde_json::Value,
    content: &AnthropicSearchResultContent,
) -> Result<WebSearchCall, ProviderError> {
    let query = require_str(input, "query")?.to_string();
    let (status, sources) = match content {
        AnthropicSearchResultContent::Results(results) => (
            SearchStatus::Completed,
            results
                .iter()
                .map(|r| {
                    let AnthropicSearchResult::WebSearchResult { url, title, .. } = r;
                    WebSearchSource::new(url, title, None)
                })
                .collect(),
        ),
        AnthropicSearchResultContent::Error(_) => (SearchStatus::Failed, Vec::new()),
    };
    Ok(WebSearchCall {
        id: id.to_string(),
        action: WebSearchAction::Search,
        query,
        status,
        sources,
    })
}

/// web_fetch 调用对投影为 OpenPage 动作（目标 URL 取自调用 input）
fn project_fetch_call(
    id: &str,
    input: &serde_json::Value,
    content: &AnthropicFetchResultContent,
) -> Result<WebSearchCall, ProviderError> {
    let url = require_str(input, "url")?.to_string();
    let status = match content {
        AnthropicFetchResultContent::Result(_) => SearchStatus::Completed,
        AnthropicFetchResultContent::Error(_) => SearchStatus::Failed,
    };
    Ok(WebSearchCall {
        id: id.to_string(),
        action: WebSearchAction::OpenPage { url },
        query: String::new(),
        status,
        sources: Vec::new(),
    })
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

/// 由原生块链与 usage 快照组装统一 ProviderResponse（流式与非流式共用）
fn assemble_response(
    blocks: Vec<AnthropicContentBlock>,
    usage: AnthropicUsageSnapshot,
) -> Result<ProviderResponse, ProviderError> {
    let parts = project_parts(&blocks)?;
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
            content_blocks: blocks,
        }),
        ..Default::default()
    };
    Ok(ProviderResponse {
        messages: vec![Message::new(parts)],
        usage,
        meta,
    })
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_native_search: true,
            supports_reasoning_effort: false,
            server_tool_names: vec!["web_search", "web_fetch"],
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
            partial_inputs: HashMap::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(event_sender),
        };
        let _ = post_sse(
            &self.normalized_endpoint(),
            self.auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;
        let (blocks, usage) = handler.finish()?;

        assemble_response(blocks, usage)
    }
}

/// Anthropic 流式事件累积处理器：还原原生 content blocks 与 usage / stop_reason 元数据。
/// 信封字段（type / index / id 等）缺失或类型不符直接报协议错误；
/// 未知的块类型 / delta 类型因意味着内容丢失，同样显式报错而非静默吞没。
struct AnthropicStreamHandler {
    blocks: Vec<AnthropicContentBlock>,
    /// tool_use / server_tool_use 的增量输入 JSON：官方规范以 input_json_delta 流式下发，
    /// 完整 JSON 由 finish 统一解析回填；server tool 结果块到达时提前解析配对
    partial_inputs: HashMap<usize, String>,
    usage: AnthropicUsageSnapshot,
    forwarder: EventForwarder,
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
    fn finish(
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
                                serde_json::from_value(raw_content.clone()).map_err(|e| {
                                    ProviderError::Protocol(format!(
                                        "Invalid web_search_tool_result content: {e}"
                                    ))
                                })?;
                            let input = self.server_use_input(&tool_use_id)?;
                            let call = project_search_call(&tool_use_id, &input, &content)?;
                            self.forwarder.web_search_call(call).await;
                            AnthropicContentBlock::WebSearchToolResult {
                                tool_use_id,
                                content,
                            }
                        } else {
                            let content: AnthropicFetchResultContent =
                                serde_json::from_value(raw_content.clone()).map_err(|e| {
                                    ProviderError::Protocol(format!(
                                        "Invalid web_fetch_tool_result content: {e}"
                                    ))
                                })?;
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
                        let citation: AnthropicCitation = serde_json::from_value(raw.clone())
                            .map_err(|e| {
                                ProviderError::Protocol(format!(
                                    "Invalid citation in citations_delta: {e}"
                                ))
                            })?;
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

fn tool_output(result: &crate::model::ToolResult) -> Result<serde_json::Value, ProviderError> {
    if result.attachments.is_empty() {
        return Ok(serde_json::json!(result.content));
    }
    let mut content = vec![serde_json::json!({"type": "text", "text": result.content})];
    for media in &result.attachments {
        crate::config::ProviderType::Anthropic
            .require_media(media.kind, &media.mime_type)
            .map_err(ProviderError::Protocol)?;
        let kind = match media.kind {
            crate::model::MediaKind::Image => "image",
            crate::model::MediaKind::Pdf => "document",
            _ => unreachable!("validated media kind"),
        };
        content.push(serde_json::json!({
            "type": kind,
            "source": {"type": "base64", "media_type": media.mime_type, "data": media.data},
        }));
    }
    Ok(serde_json::json!(content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::path::PathBuf;

    #[test]
    fn media_tool_results_use_native_content_blocks() {
        let session = super::super::common::media_tool_session();
        let body = AnthropicProvider::new("test")
            .build_request_body(&session, &[], &CompletionOptions::default())
            .unwrap();
        let output = &body["messages"][1]["content"][0];
        assert_eq!(output["tool_use_id"], "read_media");
        assert_eq!(output["content"][0]["type"], "text");
        assert_eq!(output["content"][1]["type"], "image");
        assert_eq!(output["content"][1]["source"]["media_type"], "image/png");
        assert!(
            output["content"][1]["source"]["data"]
                .as_str()
                .unwrap()
                .starts_with("iVBOR")
        );
    }

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
                attachments: Vec::new(),
                tool_call_id: "call_1".into(),
                content: "file contents".into(),
                is_error: false,
            })],
        ));
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                attachments: Vec::new(),
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
            partial_inputs: HashMap::new(),
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
        let (blocks, usage) = handler.finish().expect("finish");
        let streamed = assemble_response(blocks, usage).expect("assemble");

        assert_eq!(streamed.messages[0].parts, parsed.messages[0].parts);
        assert_eq!(streamed.usage, parsed.usage);
        assert_eq!(streamed.meta.anthropic, parsed.meta.anthropic);
    }

    #[tokio::test]
    async fn test_anthropic_stream_error_event_reports_protocol_error() {
        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            partial_inputs: HashMap::new(),
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
            partial_inputs: HashMap::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(None),
        };
        let err = handler
            .on_event(serde_json::json!({
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "future_block", "id": "x", "name": "y" }
            }))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Protocol(ref msg) if msg.contains("future_block")),
            "unexpected error: {err:?}"
        );
    }

    /// 官方 web search 响应样例：server_tool_use 与结果块配对投影为 WebSearchCall，
    /// 引用注脚投影为 WebSearchResultLocation，原生块链逐块保真回传
    #[test]
    fn test_anthropic_web_search_parse_and_native_echo() {
        let provider = AnthropicProvider::new("test-key");
        let mock_resp = serde_json::json!({
            "id": "msg_ws_1",
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "text", "text": "I'll search for when Claude Shannon was born." },
                {
                    "type": "server_tool_use",
                    "id": "srvtoolu_01WYG3ziw53XMcoyKL4XcZmE",
                    "name": "web_search",
                    "input": { "query": "claude shannon birth date" }
                },
                {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_01WYG3ziw53XMcoyKL4XcZmE",
                    "content": [
                        {
                            "type": "web_search_result",
                            "url": "https://en.wikipedia.org/wiki/Claude_Shannon",
                            "title": "Claude Shannon - Wikipedia",
                            "encrypted_content": "EqgfCioIARgBIiQ3YTAwMjY1Mi1mZjM5LTQ1NGUtODgxNC1kNjNjNTk1ZWI3Y",
                            "page_age": "April 30, 2025"
                        }
                    ]
                },
                {
                    "type": "text",
                    "text": "Claude Shannon was born on April 30, 1916, in Petoskey, Michigan",
                    "citations": [
                        {
                            "type": "web_search_result_location",
                            "url": "https://en.wikipedia.org/wiki/Claude_Shannon",
                            "title": "Claude Shannon - Wikipedia",
                            "cited_text": "Claude Elwood Shannon (April 30, 1916 - February 24, 2001)",
                            "encrypted_index": "Eo8BCioIAhgBIiQyYjQ0OWJmZi1lNm"
                        }
                    ]
                }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 100, "output_tokens": 40 }
        });

        let parsed = provider.parse_response(mock_resp).expect("parse response");
        let parts = &parsed.messages[0].parts;
        assert_eq!(parts.len(), 3);

        match &parts[1] {
            ContentPart::WebSearchCall(call) => {
                assert_eq!(call.id, "srvtoolu_01WYG3ziw53XMcoyKL4XcZmE");
                assert_eq!(call.query, "claude shannon birth date");
                assert_eq!(call.status, SearchStatus::Completed);
                assert_eq!(call.sources.len(), 1);
                assert_eq!(
                    call.sources[0].url,
                    "https://en.wikipedia.org/wiki/Claude_Shannon"
                );
            }
            _ => panic!("Expected WebSearchCall"),
        }
        match &parts[2] {
            ContentPart::Text { text, annotations } => {
                assert!(text.contains("April 30, 1916"));
                assert_eq!(
                    annotations,
                    &vec![Annotation::WebSearchResultLocation {
                        url: "https://en.wikipedia.org/wiki/Claude_Shannon".into(),
                        title: "Claude Shannon - Wikipedia".into(),
                        cited_text: "Claude Elwood Shannon (April 30, 1916 - February 24, 2001)"
                            .into(),
                    }]
                );
            }
            _ => panic!("Expected Text with citations"),
        }

        // 多轮重建：assistant 原生块链逐块原样回传（encrypted_content / encrypted_index 不变）
        let mut session = Session::new(PathBuf::from("/tmp"), None);
        session.append_turn(Turn::user_turn(Message::text("When was Shannon born?")));
        let mut asst_turn = Turn::assistant_turn(parsed.messages);
        asst_turn.meta = parsed.meta;
        session.append_turn(asst_turn);
        session.append_turn(Turn::user_turn(Message::text("Cite the source.")));

        let options = CompletionOptions {
            model: "claude-sonnet-4-5".into(),
            enable_web_search: true,
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");

        // 服务端工具声明：最新版本 + 直连调用（无 programmatic tool calling 能力）
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "web_search_20260318");
        assert_eq!(tools[0]["name"], "web_search");
        assert_eq!(tools[0]["allowed_callers"], serde_json::json!(["direct"]));
        assert_eq!(tools[1]["type"], "web_fetch_20260318");
        assert_eq!(tools[1]["name"], "web_fetch");
        assert_eq!(tools[1]["allowed_callers"], serde_json::json!(["direct"]));

        let asst_content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(
            asst_content[2]["content"][0]["encrypted_content"],
            "EqgfCioIARgBIiQ3YTAwMjY1Mi1mZjM5LTQ1NGUtODgxNC1kNjNjNTk1ZWI3Y"
        );
        assert_eq!(
            asst_content[3]["citations"][0]["encrypted_index"],
            "Eo8BCioIAhgBIiQyYjQ0OWJmZi1lNm"
        );
    }

    /// annotation 关闭 web_fetch：仅声明 web_search，
    /// 保住会被未接线 web_fetch 的中转端点整组丢弃的原生搜索
    #[test]
    fn test_anthropic_web_fetch_declaration_disabled_by_annotation() {
        let provider = AnthropicProvider::new("test-key");
        let session = Session::new(PathBuf::from("/tmp"), None);
        let options = CompletionOptions {
            model: "claude-haiku-4-5".into(),
            enable_web_search: true,
            enable_web_fetch: false,
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "web_search_20260318");
        assert_eq!(tools[0]["name"], "web_search");
    }

    /// 官方 web fetch 响应样例：调用对投影为 OpenPage，document 内容块回传保真
    #[test]
    fn test_anthropic_web_fetch_parse_and_native_echo() {
        let provider = AnthropicProvider::new("test-key");
        let mock_resp = serde_json::json!({
            "id": "msg_wf_1",
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "text", "text": "Let me fetch that article." },
                {
                    "type": "server_tool_use",
                    "id": "srvtoolu_fetch_1",
                    "name": "web_fetch",
                    "input": { "url": "https://example.com/article" }
                },
                {
                    "type": "web_fetch_tool_result",
                    "tool_use_id": "srvtoolu_fetch_1",
                    "content": {
                        "type": "web_fetch_result",
                        "url": "https://example.com/article",
                        "content": {
                            "type": "document",
                            "source": {
                                "type": "text",
                                "media_type": "text/plain",
                                "data": "Full text content of the article..."
                            },
                            "title": "Article Title"
                        },
                        "retrieved_at": "2025-08-25T10:30:00Z"
                    }
                },
                { "type": "text", "text": "The article says hi." }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 50, "output_tokens": 20 }
        });

        let parsed = provider
            .parse_response(mock_resp.clone())
            .expect("parse response");
        let parts = &parsed.messages[0].parts;
        assert_eq!(parts.len(), 3);
        match &parts[1] {
            ContentPart::WebSearchCall(call) => {
                assert_eq!(call.id, "srvtoolu_fetch_1");
                assert_eq!(
                    call.action,
                    WebSearchAction::OpenPage {
                        url: "https://example.com/article".into()
                    }
                );
                assert_eq!(call.status, SearchStatus::Completed);
                assert!(call.sources.is_empty());
            }
            _ => panic!("Expected OpenPage WebSearchCall"),
        }

        let mut session = Session::new(PathBuf::from("/tmp"), None);
        session.append_turn(Turn::user_turn(Message::text("Fetch the article")));
        let mut asst_turn = Turn::assistant_turn(parsed.messages);
        asst_turn.meta = parsed.meta;
        session.append_turn(asst_turn);
        let body = provider
            .build_request_body(
                &session,
                &[],
                &CompletionOptions {
                    model: "claude-sonnet-4-5".into(),
                    enable_web_search: true,
                    ..Default::default()
                },
            )
            .expect("build body");
        assert_eq!(
            body["messages"][1]["content"].as_array().unwrap(),
            mock_resp["content"].as_array().unwrap()
        );
    }

    /// 服务端工具执行失败仍为 HTTP 200：错误结果按 Failed 状态上报
    #[test]
    fn test_anthropic_search_error_result_marks_failed() {
        let provider = AnthropicProvider::new("test-key");
        let mock_resp = serde_json::json!({
            "id": "msg_err_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "server_tool_use",
                    "id": "srvtoolu_err",
                    "name": "web_search",
                    "input": { "query": "blocked query" }
                },
                {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_err",
                    "content": {
                        "type": "web_search_tool_result_error",
                        "error_code": "max_uses_exceeded"
                    }
                }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 30, "output_tokens": 10 }
        });
        let parsed = provider.parse_response(mock_resp).expect("parse response");
        match &parsed.messages[0].parts[0] {
            ContentPart::WebSearchCall(call) => {
                assert_eq!(call.query, "blocked query");
                assert_eq!(call.status, SearchStatus::Failed);
                assert!(call.sources.is_empty());
            }
            _ => panic!("Expected WebSearchCall"),
        }
    }

    /// 官方流式序列：server_tool_use 经 input_json_delta 增量下发，结果块整块到达，
    /// 引用经 citations_delta 逐条追加；流式与非流式产出同一条原生块链
    #[tokio::test]
    async fn test_anthropic_web_search_streaming_matches_parse() {
        let provider = AnthropicProvider::new("test-key");
        let (tx, mut rx) = mpsc::channel(16);
        let mut handler = AnthropicStreamHandler {
            blocks: Vec::new(),
            partial_inputs: HashMap::new(),
            usage: AnthropicUsageSnapshot::default(),
            forwarder: EventForwarder::new(Some(tx)),
        };
        let events = [
            serde_json::json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": 60 } }
            }),
            serde_json::json!({
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "" }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "Searching." }
            }),
            serde_json::json!({
                "type": "content_block_start", "index": 1,
                "content_block": { "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search" }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 1,
                "delta": { "type": "input_json_delta", "partial_json": "{\"query\":" }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 1,
                "delta": { "type": "input_json_delta", "partial_json": "\"rust edition\"}" }
            }),
            serde_json::json!({
                "type": "content_block_start", "index": 2,
                "content_block": {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_1",
                    "content": [
                        {
                            "type": "web_search_result",
                            "url": "https://blog.rust-lang.org",
                            "title": "Rust Blog",
                            "encrypted_content": "Enc1"
                        }
                    ]
                }
            }),
            serde_json::json!({
                "type": "content_block_start", "index": 3,
                "content_block": { "type": "text", "text": "" }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 3,
                "delta": { "type": "text_delta", "text": "Rust 2024 is out." }
            }),
            serde_json::json!({
                "type": "content_block_delta", "index": 3,
                "delta": {
                    "type": "citations_delta",
                    "citation": {
                        "type": "web_search_result_location",
                        "url": "https://blog.rust-lang.org",
                        "title": "Rust Blog",
                        "cited_text": "Rust 2024 is out",
                        "encrypted_index": "Idx1"
                    }
                }
            }),
            serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 30 }
            }),
            serde_json::json!({ "type": "message_stop" }),
        ];
        for ev in events {
            handler.on_event(ev).await.expect("handle stream event");
        }
        let (blocks, usage) = handler.finish().expect("finish");

        // 事件流按序转发：文本增量 → 搜索结果 → 文本增量 → 引用
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        assert_eq!(seen.len(), 4);
        assert!(
            matches!(&seen[0], AgentEvent::Text { text_id, delta } if text_id == "0" && delta == "Searching.")
        );
        match &seen[1] {
            AgentEvent::WebSearchCall(call) => {
                assert_eq!(call.id, "srvtoolu_1");
                assert_eq!(call.query, "rust edition");
                assert_eq!(call.status, SearchStatus::Completed);
                assert_eq!(call.sources.len(), 1);
                assert_eq!(call.sources[0].url, "https://blog.rust-lang.org");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(
            matches!(&seen[2], AgentEvent::Text { text_id, delta } if text_id == "3" && delta == "Rust 2024 is out.")
        );
        match &seen[3] {
            AgentEvent::Citation {
                text_id,
                annotation,
            } => {
                assert_eq!(text_id, "3");
                assert_eq!(
                    annotation,
                    &Annotation::WebSearchResultLocation {
                        url: "https://blog.rust-lang.org".into(),
                        title: "Rust Blog".into(),
                        cited_text: "Rust 2024 is out".into(),
                    }
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // 流式组装与非流式解析同源：领域投影与原生块链完全一致
        let streamed = assemble_response(blocks, usage).expect("assemble");
        let parsed = provider
            .parse_response(serde_json::json!({
                "id": "msg_stream_ws",
                "type": "message",
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "Searching." },
                    {
                        "type": "server_tool_use",
                        "id": "srvtoolu_1",
                        "name": "web_search",
                        "input": { "query": "rust edition" }
                    },
                    {
                        "type": "web_search_tool_result",
                        "tool_use_id": "srvtoolu_1",
                        "content": [
                            {
                                "type": "web_search_result",
                                "url": "https://blog.rust-lang.org",
                                "title": "Rust Blog",
                                "encrypted_content": "Enc1"
                            }
                        ]
                    },
                    {
                        "type": "text",
                        "text": "Rust 2024 is out.",
                        "citations": [
                            {
                                "type": "web_search_result_location",
                                "url": "https://blog.rust-lang.org",
                                "title": "Rust Blog",
                                "cited_text": "Rust 2024 is out",
                                "encrypted_index": "Idx1"
                            }
                        ]
                    }
                ],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 60, "output_tokens": 30 }
            }))
            .expect("parse full");
        assert_eq!(streamed.messages[0].parts, parsed.messages[0].parts);
        assert_eq!(streamed.meta, parsed.meta);
    }
}
