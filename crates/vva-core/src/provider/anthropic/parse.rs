use std::collections::HashMap;

use super::AnthropicProvider;
use crate::model::{
    Annotation, AnthropicCitation, AnthropicContentBlock, AnthropicFetchResultContent,
    AnthropicSearchResult, AnthropicSearchResultContent, AnthropicTurnMeta, ContentPart, Message,
    SearchStatus, TokenUsage, ToolCall, ToolDefinition, TurnMeta, WebSearchAction, WebSearchCall,
    WebSearchSource,
};
use crate::provider::common::{freeform_call_input, is_freeform_tool, require_str};
use crate::provider::{ProviderError, ProviderResponse};

impl AnthropicProvider {
    pub(super) fn parse_response(
        &self,
        json: serde_json::Value,
        tools: &[ToolDefinition],
    ) -> Result<ProviderResponse, ProviderError> {
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

        assemble_response(dto.content, usage, tools)
    }
}

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
pub(super) fn project_parts(
    blocks: &[AnthropicContentBlock],
    tools: &[ToolDefinition],
) -> Result<Vec<ContentPart>, ProviderError> {
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
                parts.push(ContentPart::ToolCall(if is_freeform_tool(tools, name) {
                    ToolCall::custom(id, name, freeform_call_input(name, input)?)
                } else {
                    ToolCall::new(id, name, input.to_string())
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

/// 取回并移除与结果块配对的 server_tool_use：缺失即报协议错误
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

pub(super) fn project_annotation(citation: &AnthropicCitation) -> Annotation {
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
pub(super) fn project_search_call(
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
        AnthropicSearchResultContent::Error(_) | AnthropicSearchResultContent::WrappedError(_) => {
            (SearchStatus::Failed, Vec::new())
        }
    };
    Ok(WebSearchCall {
        id: id.to_string(),
        action: Some(WebSearchAction::Search),
        query,
        status,
        sources,
    })
}

/// web_fetch 调用对投影为 OpenPage 动作（目标 URL 取自调用 input）
pub(super) fn project_fetch_call(
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
        action: Some(WebSearchAction::OpenPage { url }),
        query: String::new(),
        status,
        sources: Vec::new(),
    })
}

/// usage / stop_reason 快照：流式逐步累积，非流式一次性提取
#[derive(Default)]
pub(super) struct AnthropicUsageSnapshot {
    pub(super) seen: bool,
    pub(super) input_tokens: u32,
    pub(super) output_tokens: u32,
    pub(super) cache_creation_input_tokens: Option<u32>,
    pub(super) cache_read_input_tokens: Option<u32>,
    pub(super) stop_reason: Option<String>,
}

/// 由原生块链与 usage 快照组装统一 ProviderResponse（流式与非流式共用）
pub(super) fn assemble_response(
    blocks: Vec<AnthropicContentBlock>,
    usage: AnthropicUsageSnapshot,
    tools: &[ToolDefinition],
) -> Result<ProviderResponse, ProviderError> {
    let parts = project_parts(&blocks, tools)?;
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
            scope: None,
        }),
        ..Default::default()
    };
    Ok(ProviderResponse {
        messages: vec![Message::new(parts)],
        usage,
        meta,
    })
}
