use super::AnthropicProvider;
use crate::config::ProviderType;
use crate::model::{
    ContentPart, MediaKind, MediaSource, Message, Role, Session, ToolDefinition, TurnMeta,
};
use crate::provider::Provider;
use crate::provider::common::{RoleBlockMerger, WebCallSurface, web_call_surface};
use crate::provider::common::{function_parameters, replay_call_args};
use crate::provider::{CompletionOptions, ProviderError};

/// web_search / web_fetch 服务端工具的默认版本后缀（模型级 `annotation.web_tool_version` 可覆写）
const LATEST_WEB_TOOL_VERSION: &str = "20260318";

impl AnthropicProvider {
    pub(super) fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<serde_json::Value, ProviderError> {
        let mut api_tools: Vec<serde_json::Value> = Vec::new();
        // 核心原生优势：服务端代执行的 web_search / web_fetch（引用与结果块由服务端直接产出）
        // _20260209 起默认经代码执行容器调用，显式声明直连。
        let version = options
            .web_tool_version
            .as_deref()
            .unwrap_or(LATEST_WEB_TOOL_VERSION);
        let direct_callers = serde_json::json!(["direct"]);
        if options.enable_web_search {
            api_tools.push(serde_json::json!({
                "type": format!("web_search_{version}"),
                "name": "web_search",
                "allowed_callers": direct_callers,
            }));
        }
        if options.enable_web_fetch {
            api_tools.push(serde_json::json!({
                "type": format!("web_fetch_{version}"),
                "name": "web_fetch",
                "allowed_callers": direct_callers,
            }));
        }
        api_tools.extend(tools.iter().map(|tool| {
            serde_json::json!({"name": tool.name, "description": tool.description, "input_schema": function_parameters(&tool.input)})
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
        let scope = options.affinity.as_deref();

        for (i, (role, msg, meta)) in flattened_msgs.iter().enumerate() {
            let should_cache = i + 1 == total_msgs && *role == Role::User;

            match role {
                Role::System => {
                    system_text.push_str(&msg.text_content());
                }
                Role::User => {
                    let mut content_blocks = Vec::new();
                    for part in &msg.parts {
                        match part {
                            ContentPart::Text { text, .. } => {
                                let block = serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                });
                                content_blocks.push(block);
                            }
                            ContentPart::File {
                                source: MediaSource::Base64 { data, mime_type },
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
                                MediaSource::Base64 { data, mime_type } => {
                                    content_blocks.push(serde_json::json!({
                                        "type": "image",
                                        "source": {
                                            "type": "base64",
                                            "media_type": mime_type,
                                            "data": data,
                                        }
                                    }));
                                }
                                MediaSource::Url { url } => {
                                    content_blocks.push(serde_json::json!({
                                        "type": "image",
                                        "source": {
                                            "type": "url",
                                            "url": url,
                                        }
                                    }));
                                }
                            },
                            ContentPart::FileRef {
                                path,
                                content,
                                truncated,
                            } => {
                                let block = serde_json::json!({
                                    "type": "text",
                                    "text": crate::model::format_file_ref(path, content, *truncated),
                                });
                                content_blocks.push(block);
                            }
                            _ => {}
                        }
                    }
                    if should_cache && let Some(last) = content_blocks.last_mut() {
                        last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
                    }
                    merger.push("user", content_blocks);
                }
                Role::Assistant => {
                    // 核心原生优势：原生块链保真回传（encrypted_content / encrypted_index
                    // 不可重建，逐块原样带回），同 Responses output_items 策略。
                    // 块链绑定签发时的模型亲和组，跨组（切换模型）回传即失真，降维为通用重建
                    if let Some(anthropic) = meta.anthropic.as_ref()
                        && anthropic.scope.as_deref() == scope
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
                    // 搜索调用以客户端函数形态重建时，tool_result 必须挂在紧随的 user 消息上
                    let mut web_tool_results = Vec::new();
                    for part in &msg.parts {
                        match part {
                            // Reasoning/thinking 绑定签发者（签名跨亲和组无效），重建时丢弃
                            ContentPart::Text { text, .. } => {
                                content_blocks.push(serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                }));
                            }
                            ContentPart::ToolCall(call) => {
                                let args_val = replay_call_args(call)?;
                                content_blocks.push(serde_json::json!({
                                    "type": "tool_use",
                                    "id": call.id,
                                    "name": call.name,
                                    "input": args_val,
                                }));
                            }
                            ContentPart::WebSearchCall(call) => {
                                // 跨亲和组重建：搜索调用按当前工具面投影，二者皆无则丢弃
                                match web_call_surface(
                                    call,
                                    options,
                                    tools,
                                    &self.capabilities().server_tool_names,
                                ) {
                                    Some(WebCallSurface::Native(name, input)) => {
                                        let result_type = if name == "web_fetch" {
                                            "web_fetch_tool_result"
                                        } else {
                                            "web_search_tool_result"
                                        };
                                        content_blocks.push(serde_json::json!({
                                            "type": "server_tool_use",
                                            "id": call.id,
                                            "name": name,
                                            "input": input,
                                        }));
                                        // 官方 tool result 允许字符串 content；加密快照
                                        // 属签发者状态，无法跨组携带，以摘要文本替代
                                        content_blocks.push(serde_json::json!({
                                            "type": result_type,
                                            "tool_use_id": call.id,
                                            "content": call.result_text(),
                                        }));
                                    }
                                    Some(WebCallSurface::Client(name, input)) => {
                                        content_blocks.push(serde_json::json!({
                                            "type": "tool_use",
                                            "id": call.id,
                                            "name": name,
                                            "input": input,
                                        }));
                                        web_tool_results.push(serde_json::json!({
                                            "type": "tool_result",
                                            "tool_use_id": call.id,
                                            "content": call.result_text(),
                                            "is_error": false,
                                        }));
                                    }
                                    None => {}
                                }
                            }
                            _ => {}
                        }
                    }
                    merger.push("assistant", content_blocks);
                    if !web_tool_results.is_empty() {
                        merger.push("user", web_tool_results);
                    }
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

        if options.disable_thinking {
            body["thinking"] = serde_json::json!({ "type": "disabled" });
        }

        // 原生 structured outputs：无需 beta header，JSON 文本落在 text 块中
        if let Some(rs) = &options.response_schema {
            body["output_config"] = serde_json::json!({
                "format": { "type": "json_schema", "schema": rs.schema }
            });
        }

        Ok(body)
    }
}

fn tool_output(result: &crate::model::ToolResult) -> Result<serde_json::Value, ProviderError> {
    if result.attachments.is_empty() {
        return Ok(serde_json::json!(result.content));
    }
    let mut content = vec![serde_json::json!({"type": "text", "text": result.content})];
    for media in &result.attachments {
        ProviderType::Anthropic
            .require_media(media.kind, &media.mime_type)
            .map_err(ProviderError::Protocol)?;
        let kind = match media.kind {
            MediaKind::Image => "image",
            MediaKind::Pdf => "document",
            _ => unreachable!("validated media kind"),
        };
        content.push(serde_json::json!({
            "type": kind,
            "source": {"type": "base64", "media_type": media.mime_type, "data": media.data},
        }));
    }
    Ok(serde_json::json!(content))
}
