use super::AnthropicProvider;
use crate::config::ProviderType;
use crate::model::{
    ContentPart, MediaKind, MediaSource, Message, Role, Session, ToolDefinition, TurnMeta,
};
use crate::provider::common::RoleBlockMerger;
use crate::provider::{CompletionOptions, ProviderError};

impl AnthropicProvider {
    pub(super) fn build_request_body(
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
