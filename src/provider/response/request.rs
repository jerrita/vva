use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

use super::ResponseProvider;

use crate::config::ProviderType;
use crate::model::{ContentPart, MediaKind, MediaSource, Role, Session, ToolDefinition};
use crate::provider::{CompletionOptions, ProviderError};

impl ResponseProvider {
    /// 规范化请求端点：base_url 无论是否携带 /responses 后缀，均收敛到唯一端点
    pub(super) fn normalized_endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/responses").unwrap_or(base);
        format!("{base}/responses")
    }

    /// Bearer 鉴权与 JSON Content-Type 头
    pub(super) fn build_auth_headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .map_err(|e| ProviderError::Protocol(e.to_string()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    /// 将 Session 与当前上下文转换为 Responses API 请求体
    pub(super) fn build_request_body(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<serde_json::Value, ProviderError> {
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

        // 存在 previous_response_id 时仅增量发送该 response_id 之后的轮次；
        // 定位不到锚点轮次或其后无新轮次时回退全量历史
        let turns_to_send = prev_id
            .and_then(|prev| {
                let pos = session
                    .turns
                    .iter()
                    .position(|turn| turn.openai_response_id() == Some(prev))?;
                let delta = &session.turns[pos + 1..];
                (!delta.is_empty()).then_some(delta)
            })
            .unwrap_or(&session.turns);

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
                match turn.role {
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
                                ContentPart::Image { source, .. } => {
                                    let url = match source {
                                        MediaSource::Url { url } => url.clone(),
                                        MediaSource::Base64 { data, mime_type } => {
                                            format!("data:{mime_type};base64,{data}")
                                        }
                                    };
                                    content_arr.push(serde_json::json!({
                                        "type": "input_image", "image_url": url,
                                    }));
                                }
                                ContentPart::File {
                                    source: MediaSource::Base64 { data, mime_type },
                                    filename,
                                    ..
                                } => {
                                    content_arr.push(serde_json::json!({
                                        "type": "input_file",
                                        "filename": filename,
                                        "file_data": format!("data:{mime_type};base64,{data}"),
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
                                    "output": tool_output(res)?,
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

        if let Some(tier) = &options.service_tier {
            body["service_tier"] = serde_json::Value::String(tier.clone());
        }

        if let Some(effort) = &options.reasoning_effort
            && !effort.is_empty()
        {
            // OpenAI 仅在显式请求 summary 时才生成思考摘要，否则 reasoning item 的 summary 恒为空
            body["reasoning"] = serde_json::json!({
                "effort": effort,
                "summary": "concise"
            });
        }

        Ok(body)
    }
}

fn tool_output(result: &crate::model::ToolResult) -> Result<serde_json::Value, ProviderError> {
    if result.attachments.is_empty() {
        return Ok(serde_json::json!(result.content));
    }
    let mut content = vec![serde_json::json!({"type": "input_text", "text": result.content})];
    for media in &result.attachments {
        ProviderType::Response
            .require_media(media.kind, &media.mime_type)
            .map_err(ProviderError::Protocol)?;
        let data_url = format!("data:{};base64,{}", media.mime_type, media.data);
        content.push(match media.kind {
            MediaKind::Image => serde_json::json!({"type": "input_image", "image_url": data_url}),
            MediaKind::Pdf => serde_json::json!({"type": "input_file", "filename": media.filename, "file_data": data_url}),
            _ => unreachable!("validated media kind"),
        });
    }
    Ok(serde_json::json!(content))
}
