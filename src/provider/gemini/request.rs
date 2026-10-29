use super::GeminiProvider;
use crate::model::{ContentPart, MediaSource, Role, Session, ToolDefinition};
use crate::provider::common::RoleBlockMerger;
use crate::provider::common::{function_parameters, replay_call_args};
use crate::provider::{CompletionOptions, ProviderError};

impl GeminiProvider {
    /// 将 Session 转换为 Gemini API 的 contents 与 tools 结构
    pub(super) fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<serde_json::Value, ProviderError> {
        // Gemini 要求连续同角色内容聚合进同一条 content，仅与 Anthropic 的字段名不同
        let mut merger = RoleBlockMerger::new("parts");
        let mut system_instruction_parts = Vec::new();

        for turn in &session.turns {
            if turn.role == Role::Assistant
                && let Some(meta) = &turn.meta.gemini
                && meta.scope.as_deref() == options.affinity.as_deref()
                && !meta.content_parts.is_empty()
            {
                let parts = meta
                    .content_parts
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| {
                        ProviderError::Protocol(format!("Serialize Gemini content parts: {e}"))
                    })?;
                merger.push("model", parts);
                continue;
            }
            for msg in &turn.messages {
                match turn.role {
                    Role::System => {
                        system_instruction_parts.push(serde_json::json!({
                            "text": msg.text_content()
                        }));
                    }
                    Role::User => {
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
                                ContentPart::Image {
                                    source: MediaSource::Base64 { data, mime_type },
                                    ..
                                }
                                | ContentPart::File {
                                    source: MediaSource::Base64 { data, mime_type },
                                    ..
                                } => {
                                    parts_arr.push(serde_json::json!({
                                        "inlineData": {
                                            "mimeType": mime_type,
                                            "data": data
                                        }
                                    }));
                                }
                                ContentPart::FileRef {
                                    path,
                                    content,
                                    truncated,
                                } => {
                                    parts_arr.push(serde_json::json!({
                                        "text": crate::model::format_file_ref(path, content, *truncated)
                                    }));
                                }
                                _ => {}
                            }
                        }
                        merger.push("user", parts_arr);
                    }
                    Role::Assistant => {
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
                                ContentPart::ToolCall(call) => {
                                    let args = replay_call_args(call)?;
                                    parts_arr.push(serde_json::json!({
                                        "functionCall": {
                                            "name": call.name,
                                            "args": args
                                        }
                                    }));
                                }
                                _ => {}
                            }
                        }
                        merger.push("model", parts_arr);
                    }
                    Role::Tool => {
                        let mut parts_arr = Vec::new();
                        for part in &msg.parts {
                            if let ContentPart::ToolResult(res) = part {
                                // 查找该 tool_call_id 对应的实际函数名称（缺失即会话数据损坏，按协议错误上报）
                                let function_name = session
                                    .find_tool_call(&res.tool_call_id)
                                    .map(|c| c.name.clone())
                                    .ok_or_else(|| {
                                        ProviderError::Protocol(format!(
                                            "No ToolCall '{}' found for functionResponse",
                                            res.tool_call_id
                                        ))
                                    })?;

                                parts_arr.push(serde_json::json!({
                                    "functionResponse": {
                                        "name": function_name,
                                        "response": {"content": res.content, "is_error": res.is_error},
                                    }
                                }));
                                // Inline media in the same user content also supports models
                                // that accept media inputs but not FunctionResponse.parts.
                                for media in &res.attachments {
                                    parts_arr.push(serde_json::json!({
                                        "inlineData": {"mimeType": media.mime_type, "data": media.data}
                                    }));
                                }
                            }
                        }
                        merger.push("user", parts_arr);
                    }
                }
            }
        }

        let mut body = serde_json::json!({
            "contents": merger.into_messages(),
        });

        if !system_instruction_parts.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": system_instruction_parts
            });
        }

        let mut tools_list: Vec<serde_json::Value> = Vec::new();

        // 核心原生优势：开启 Google 官方实时 Grounding
        if options.enable_web_search {
            tools_list.push(serde_json::json!({
                "google_search": {}
            }));
        }

        if options.enable_web_fetch {
            tools_list.push(serde_json::json!({
                "url_context": {}
            }));
        }

        // 内置工具与函数调用混用时，官方要求显式开启服务端工具调用回传
        if !tools_list.is_empty() && !tools.is_empty() {
            body["toolConfig"] = serde_json::json!({
                "includeServerSideToolInvocations": true
            });
        }

        if !tools.is_empty() {
            let function_declarations: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| serde_json::json!({"name": t.name, "description": t.description, "parameters": function_parameters(&t.input)}))
                .collect();
            tools_list.push(serde_json::json!({
                "functionDeclarations": function_declarations
            }));
        }

        if !tools_list.is_empty() {
            body["tools"] = serde_json::Value::Array(tools_list);
        }

        let mut gen_config = serde_json::Map::new();
        if let Some(temp) = options.temperature {
            gen_config.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(max_tokens) = options.max_tokens {
            gen_config.insert("maxOutputTokens".to_string(), serde_json::json!(max_tokens));
        }
        if options.disable_thinking {
            gen_config.insert(
                "thinkingConfig".to_string(),
                serde_json::json!({ "thinkingBudget": 0 }),
            );
        } else if let Some(effort) = &options.reasoning_effort
            && !effort.is_empty()
        {
            gen_config.insert(
                "thinkingConfig".to_string(),
                serde_json::json!({ "thinkingLevel": effort, "includeThoughts": true }),
            );
        }
        if let Some(rs) = &options.response_schema {
            gen_config.insert(
                "responseMimeType".to_string(),
                serde_json::json!("application/json"),
            );
            gen_config.insert("responseJsonSchema".to_string(), rs.schema.clone());
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = serde_json::Value::Object(gen_config);
        }

        Ok(body)
    }
}
