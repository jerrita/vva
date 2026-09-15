use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::mpsc;

use super::common::{
    EventForwarder, require_array, require_bool, require_str, require_u64, require_value,
};
use super::http::{SseEventHandler, post_json, post_sse};
use super::{CompletionOptions, Provider, ProviderCapabilities, ProviderError, ProviderResponse};
use crate::event::AgentEvent;
use crate::model::{
    Annotation, ContentPart, Message, ResponseMessageContent, ResponseOutputItem,
    ResponseReasoningContent, ResponseReasoningSummary, ResponseTurnMeta, ResponseWebSearchAction,
    Role, SearchStatus, Session, TokenUsage, ToolCall, ToolDefinition, TurnMeta, WebSearchAction,
    WebSearchCall, WebSearchSource,
};

pub struct ResponseProvider {
    api_key: String,
    base_url: String,
}

impl ResponseProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 规范化请求端点：base_url 无论是否携带 /responses 后缀，均收敛到唯一端点
    fn normalized_endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/responses").unwrap_or(base);
        format!("{base}/responses")
    }

    /// Bearer 鉴权与 JSON Content-Type 头
    fn build_auth_headers(&self) -> Result<HeaderMap, ProviderError> {
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
    fn build_request_body(
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
                                        crate::model::MediaSource::Url { url } => url.clone(),
                                        crate::model::MediaSource::Base64 { data, mime_type } => {
                                            format!("data:{mime_type};base64,{data}")
                                        }
                                    };
                                    content_arr.push(serde_json::json!({
                                        "type": "input_image", "image_url": url,
                                    }));
                                }
                                ContentPart::File {
                                    source: crate::model::MediaSource::Base64 { data, mime_type },
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

    /// 解析 Responses API 响应体：先反序列化为强类型 output items，
    /// 再投影为领域 ContentPart（解析失败即报协议错误，不做字段兜底）
    fn parse_response(&self, json: serde_json::Value) -> Result<ProviderResponse, ProviderError> {
        let is_stored = match json.get("store") {
            Some(_) => require_bool(&json, "store")?,
            None => false,
        };

        let raw_id = require_str(&json, "id")?;

        let response_id = if is_stored {
            Some(raw_id.to_string())
        } else {
            None
        };

        let status = require_str(&json, "status")?.to_string();

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

        let output_items = require_array(&json, "output")?;

        let mut native_output_items = Vec::with_capacity(output_items.len());
        for item in output_items {
            let native_item: ResponseOutputItem =
                serde_json::from_value(item.clone()).map_err(|e| {
                    ProviderError::Protocol(format!("Invalid Responses API output item: {e}"))
                })?;
            native_output_items.push(native_item);
        }

        let mut parts = Vec::new();

        for item in &native_output_items {
            match item {
                ResponseOutputItem::WebSearchCall { id, status, action } => {
                    parts.push(ContentPart::WebSearchCall(project_web_search_call(
                        id,
                        status,
                        action.as_ref(),
                    )?));
                }
                ResponseOutputItem::Reasoning {
                    summary, content, ..
                } => {
                    let mut texts = Vec::new();
                    for s in summary {
                        let ResponseReasoningSummary::SummaryText { text } = s;
                        texts.push(text.clone());
                    }
                    for c in content {
                        let ResponseReasoningContent::ReasoningText { text } = c;
                        texts.push(text.clone());
                    }
                    // 各 summary/content 段以空行分隔（markdown 段落边界），渲染时独立成行
                    let reasoning_text = texts.join("\n\n");
                    if !reasoning_text.is_empty() {
                        parts.push(ContentPart::Reasoning {
                            content: reasoning_text,
                            signature: None,
                        });
                    }
                }
                ResponseOutputItem::Message { content, .. } => {
                    for c in content {
                        match c {
                            ResponseMessageContent::OutputText { text, annotations } => {
                                parts.push(ContentPart::Text {
                                    text: text.clone(),
                                    annotations: annotations.clone(),
                                });
                            }
                            ResponseMessageContent::Refusal { refusal } => {
                                parts.push(ContentPart::text(refusal.clone()));
                            }
                        }
                    }
                }
                ResponseOutputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    parts.push(ContentPart::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    }));
                }
            }
        }

        let mut usage = None;
        if let Some(u) = json.get("usage")
            && !u.is_null()
        {
            let input_tokens = require_u64(u, "input_tokens")? as u32;
            let output_tokens = require_u64(u, "output_tokens")? as u32;
            let total_tokens = require_u64(u, "total_tokens")? as u32;

            let input_details = require_value(u, "input_tokens_details")?;
            let cached = require_u64(input_details, "cached_tokens")? as u32;

            let output_details = require_value(u, "output_tokens_details")?;
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

        // 领域实体因果时序：web_search 是服务端自主执行的前置检索动作，
        // 最终回答文本（Message/Text）基于检索结果生成并携带引用注脚。
        // 部分供应商（如 Grok Responses API）在 output 数组中将 Message 置于 WebSearchCall 之前，
        // 此处校准领域模型 parts 顺序，使前置搜索排在最终回答之前。
        if let Some(first_text_pos) = parts
            .iter()
            .position(|p| matches!(p, ContentPart::Text { .. }))
            && parts[first_text_pos + 1..]
                .iter()
                .any(|p| matches!(p, ContentPart::WebSearchCall(_)))
        {
            let mut searches_after_text = Vec::new();
            let mut other_parts = Vec::with_capacity(parts.len());
            for (idx, part) in parts.into_iter().enumerate() {
                if idx > first_text_pos && matches!(part, ContentPart::WebSearchCall(_)) {
                    searches_after_text.push(part);
                } else {
                    other_parts.push(part);
                }
            }
            let mut sorted_parts =
                Vec::with_capacity(other_parts.len() + searches_after_text.len());
            sorted_parts.extend(other_parts.drain(..first_text_pos));
            sorted_parts.extend(searches_after_text);
            sorted_parts.extend(other_parts);
            parts = sorted_parts;
        }

        let message = Message::new(parts);

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
            supports_reasoning_effort: true,
            server_tool_names: vec!["web_search"],
        }
    }

    async fn complete(
        &self,
        session: &Session,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        let body = self.build_request_body(session, tools, options)?;
        let response = post_json(
            &self.normalized_endpoint(),
            self.build_auth_headers()?,
            &body,
        )
        .await?;
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

        let mut handler = ResponseStreamHandler::new(self, event_sender);
        let termination = post_sse(
            &self.normalized_endpoint(),
            self.build_auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;

        handler.completed.ok_or_else(|| {
            let response_id = handler.response_id.as_deref().unwrap_or("unknown");
            let last_event = handler.last_event_type.as_deref().unwrap_or("none");
            let sequence = handler
                .last_sequence_number
                .map_or_else(|| "unknown".into(), |value| value.to_string());
            ProviderError::Protocol(format!(
                "Stream ended without response.completed (termination={termination}, response_id={response_id}, last_event={last_event}, sequence_number={sequence}, event_count={})",
                handler.event_count
            ))
        })
    }
}

/// Responses API 流式事件累积处理器
struct ResponseStreamHandler<'a> {
    provider: &'a ResponseProvider,
    forwarder: EventForwarder,
    completed: Option<ProviderResponse>,
    response_id: Option<String>,
    last_event_type: Option<String>,
    last_sequence_number: Option<u64>,
    event_count: u64,
    /// `output_item.done` 投影：领域 WebSearchCall 的条目快照（含当时 sources）
    streamed_searches: HashMap<String, WebSearchCall>,
    warned_missing_search_actions: HashSet<String>,
}

impl<'a> ResponseStreamHandler<'a> {
    fn new(provider: &'a ResponseProvider, event_sender: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self {
            provider,
            forwarder: EventForwarder::new(event_sender),
            completed: None,
            response_id: None,
            last_event_type: None,
            last_sequence_number: None,
            event_count: 0,
            streamed_searches: HashMap::new(),
            warned_missing_search_actions: HashSet::new(),
        }
    }

    async fn warn_missing_search_action(&mut self, id: &str) {
        if self.warned_missing_search_actions.insert(id.to_string()) {
            self.forwarder
                .notice(format!(
                    "Responses API web_search_call '{id}' omitted action; displaying an empty search."
                ))
                .await;
        }
    }

    /// 领域 WebSearchCall 用 item.done 快照；native `output_items` 仍是 completed.output。
    fn apply_streamed_searches(&self, parsed: &mut ProviderResponse) {
        for part in parsed
            .messages
            .iter_mut()
            .flat_map(|message| message.parts.iter_mut())
        {
            let ContentPart::WebSearchCall(call) = part else {
                continue;
            };
            if let Some(done) = self.streamed_searches.get(&call.id) {
                call.clone_from(done);
            }
        }
    }
}

#[async_trait]
impl SseEventHandler for ResponseStreamHandler<'_> {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError> {
        let ev_type = require_str(&json, "type")?;
        self.event_count += 1;
        self.last_event_type = Some(ev_type.to_string());
        self.last_sequence_number = json.get("sequence_number").and_then(|value| value.as_u64());
        if let Some(response_id) = json
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(|id| id.as_str())
        {
            self.response_id = Some(response_id.to_string());
        }
        match ev_type {
            "error" => {
                let msg = require_str(&json, "message")?;
                Err(ProviderError::Protocol(format!("Stream error: {msg}")))
            }
            "response.failed" => {
                let resp_obj = require_value(&json, "response")?;
                let err_msg = resp_obj
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Response generation failed");
                Err(ProviderError::Protocol(format!(
                    "Response generation failed: {err_msg}"
                )))
            }
            "response.output_text.delta" => {
                let delta = require_str(&json, "delta")?;
                let text_id = response_text_id(&json)?;
                self.forwarder.text(&text_id, delta).await;
                Ok(())
            }
            "response.output_text.annotation.added" => {
                let text_id = response_text_id(&json)?;
                let annotation = require_value(&json, "annotation")?;
                if require_str(annotation, "type")? == "url_citation" {
                    let annotation: Annotation = serde_json::from_value(annotation.clone())
                        .map_err(|e| {
                            ProviderError::Protocol(format!("Invalid URL citation: {e}"))
                        })?;
                    self.forwarder.citation(&text_id, annotation).await;
                }
                Ok(())
            }
            "response.reasoning_summary_part.added" => {
                // 官方规范：summary 分多段时每段以该事件起始。各段语义独立，
                // 以空行（markdown 段落边界）分隔，避免渲染时被软换行合并成一行
                let summary_index = require_u64(&json, "summary_index")?;
                if summary_index > 0 {
                    self.forwarder.thought("\n\n").await;
                }
                Ok(())
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let delta = require_str(&json, "delta")?;
                self.forwarder.thought(delta).await;
                Ok(())
            }
            "response.output_item.done" => {
                let item = require_value(&json, "item")?;
                if require_str(item, "type")? == "web_search_call" {
                    let item: ResponseOutputItem =
                        serde_json::from_value(item.clone()).map_err(|e| {
                            ProviderError::Protocol(format!(
                                "Invalid Responses API output item: {e}"
                            ))
                        })?;
                    if let ResponseOutputItem::WebSearchCall { id, status, action } = item {
                        let missing_action = action.is_none();
                        let call = project_web_search_call(&id, &status, action.as_ref())?;
                        self.streamed_searches.insert(call.id.clone(), call.clone());
                        self.forwarder.web_search_call(call).await;
                        if missing_action {
                            self.warn_missing_search_action(&id).await;
                        }
                    }
                }
                Ok(())
            }
            "response.completed" => {
                let resp_obj = require_value(&json, "response")?;
                let mut parsed = self.provider.parse_response(resp_obj.clone())?;
                let missing_action_ids = parsed
                    .meta
                    .response
                    .iter()
                    .flat_map(|meta| &meta.output_items)
                    .filter_map(|item| match item {
                        ResponseOutputItem::WebSearchCall {
                            id, action: None, ..
                        } => Some(id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                for id in missing_action_ids {
                    self.warn_missing_search_action(&id).await;
                }
                self.apply_streamed_searches(&mut parsed);
                self.completed = Some(parsed);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn response_text_id(json: &serde_json::Value) -> Result<String, ProviderError> {
    Ok(format!(
        "{}:{}",
        require_str(json, "item_id")?,
        require_u64(json, "content_index")?
    ))
}

/// 流式条目和最终响应共用同一领域投影。
fn project_web_search_call(
    id: &str,
    status: &str,
    action: Option<&ResponseWebSearchAction>,
) -> Result<WebSearchCall, ProviderError> {
    let status = match status {
        "in_progress" | "searching" => SearchStatus::Searching,
        "completed" => SearchStatus::Completed,
        "failed" => SearchStatus::Failed,
        _ => {
            return Err(ProviderError::Protocol(format!(
                "Invalid web search status: {status}"
            )));
        }
    };
    let (action, query, sources) = match action {
        Some(ResponseWebSearchAction::Search {
            query,
            queries,
            sources,
        }) => (
            WebSearchAction::Search,
            query.clone().unwrap_or_else(|| queries.join(", ")),
            sources
                .iter()
                .map(|src| WebSearchSource::new(&src.url, &src.url, None))
                .collect(),
        ),
        Some(ResponseWebSearchAction::OpenPage { url }) => (
            WebSearchAction::OpenPage {
                url: url.clone().unwrap_or_default(),
            },
            String::new(),
            Vec::new(),
        ),
        Some(ResponseWebSearchAction::FindInPage { url, pattern }) => (
            WebSearchAction::FindInPage {
                url: url.clone(),
                pattern: pattern.clone(),
            },
            String::new(),
            Vec::new(),
        ),
        None => (WebSearchAction::Search, String::new(), Vec::new()),
    };
    Ok(WebSearchCall {
        id: id.to_string(),
        action,
        query,
        status,
        sources,
    })
}

fn tool_output(result: &crate::model::ToolResult) -> Result<serde_json::Value, ProviderError> {
    if result.attachments.is_empty() {
        return Ok(serde_json::json!(result.content));
    }
    let mut content = vec![serde_json::json!({"type": "input_text", "text": result.content})];
    for media in &result.attachments {
        crate::config::ProviderType::Response
            .require_media(media.kind, &media.mime_type)
            .map_err(ProviderError::Protocol)?;
        let data_url = format!("data:{};base64,{}", media.mime_type, media.data);
        content.push(match media.kind {
            crate::model::MediaKind::Image => serde_json::json!({"type": "input_image", "image_url": data_url}),
            crate::model::MediaKind::Pdf => serde_json::json!({"type": "input_file", "filename": media.filename, "file_data": data_url}),
            _ => unreachable!("validated media kind"),
        });
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
        let body = ResponseProvider::new("test")
            .build_request_body(&session, &[], &CompletionOptions::default())
            .unwrap();
        let output = &body["input"][1]["output"];
        assert_eq!(body["input"][1]["call_id"], "read_media");
        assert_eq!(output[0]["type"], "input_text");
        assert_eq!(output[1]["type"], "input_image");
        assert!(
            output[1]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,iVBOR")
        );
    }

    #[test]
    fn test_response_provider_payload_construction_and_previous_id() {
        let provider = ResponseProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        let user_msg = Message::text("Check the weather in Tokyo");
        let user_turn = Turn::user_turn(user_msg);
        session.append_turn(user_turn);

        // 第一轮：无 previous_response_id，携带 reasoning_effort
        let options = CompletionOptions {
            model: "gpt-4.5-preview".into(),
            enable_web_search: true,
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        let body1 = provider
            .build_request_body(&session, &[], &options)
            .expect("build body1");
        assert!(body1.get("previous_response_id").is_none());
        assert!(body1.get("max_output_tokens").is_none());
        assert_eq!(body1["tools"][0]["type"], "web_search");
        // Responses API 标准：reasoning.effort，禁止 top-level reasoning_effort
        assert_eq!(body1["reasoning"]["effort"], "medium");
        // 思考摘要需显式请求，否则 reasoning item 的 summary 恒为空
        assert_eq!(body1["reasoning"]["summary"], "concise");
        assert!(body1.get("reasoning_effort").is_none());

        // 模拟第一轮 Assistant 成功返回 response_id = "resp_turn_1"
        let mut asst_turn =
            Turn::assistant_turn(vec![Message::new(vec![ContentPart::text("It is sunny.")])]);
        asst_turn.meta.response = Some(ResponseTurnMeta::new("resp_turn_1"));
        session.append_turn(asst_turn);

        // 用户提出第二轮需求
        let user_msg2 = Message::text("What about tomorrow?");
        session.append_turn(Turn::user_turn(user_msg2));

        // 第二轮：应自动提取 previous_response_id: "resp_turn_1"
        let body2 = provider
            .build_request_body(&session, &[], &options)
            .expect("build body2");
        assert_eq!(body2["previous_response_id"], "resp_turn_1");
        // 增量 input 只包含第二轮的用户提问
        let inputs = body2["input"].as_array().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["content"][0]["text"], "What about tomorrow?");
        assert_eq!(body2["reasoning"]["effort"], "medium");
        assert_eq!(body2["reasoning"]["summary"], "concise");
    }

    #[test]
    fn test_response_provider_previous_id_with_tool_call_output_pair() {
        let provider = ResponseProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // 1. 用户提问
        session.append_turn(Turn::user_turn(Message::text("run dir")));

        // 2. Assistant 返回 ToolCall，并生成 response_id
        let mut asst_turn =
            Turn::assistant_turn(vec![Message::new(vec![ContentPart::ToolCall(ToolCall {
                id: "call_abc123".into(),
                name: "powershell".into(),
                arguments: r#"{"command":"dir"}"#.into(),
            })])]);
        asst_turn.meta.response = Some(ResponseTurnMeta::new("resp_asst_1"));
        session.append_turn(asst_turn);

        // 3. 执行工具并返回 ToolResult
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                attachments: Vec::new(),
                tool_call_id: "call_abc123".into(),
                content: "dir output".into(),
                is_error: false,
            })],
        ));

        let options = CompletionOptions {
            model: "gpt-5.6-terra".into(),
            ..Default::default()
        };

        // 4. 构建下次请求 Payload：需携带 previous_response_id，且 input 必须同时包含 function_call 和 function_call_output
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");
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
    fn test_response_provider_parse_orders_search_before_message() {
        let provider = ResponseProvider::new("test-key");
        let mock_json = serde_json::json!({
            "id": "resp_grok_1",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "status": "completed",
                    "summary": [{"type": "summary_text", "text": "Analyzing user query."}]
                },
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "Here is the summary."}]
                },
                {
                    "type": "web_search_call",
                    "id": "call_s1",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "Claude Code updates",
                        "sources": [{"type": "url", "url": "https://code.claude.com"}]
                    }
                }
            ]
        });

        let parsed = provider.parse_response(mock_json).expect("parse response");
        let parts = &parsed.messages[0].parts;
        assert_eq!(parts.len(), 3);
        assert!(matches!(parts[0], ContentPart::Reasoning { .. }));
        assert!(matches!(parts[1], ContentPart::WebSearchCall(_)));
        assert!(matches!(parts[2], ContentPart::Text { .. }));

        // 强类型原生 output_items 顺序保持与厂商完全一致
        let meta_items = &parsed.meta.response.as_ref().unwrap().output_items;
        assert_eq!(meta_items.len(), 3);
        assert!(matches!(
            meta_items[0],
            ResponseOutputItem::Reasoning { .. }
        ));
        assert!(matches!(meta_items[1], ResponseOutputItem::Message { .. }));
        assert!(matches!(
            meta_items[2],
            ResponseOutputItem::WebSearchCall { .. }
        ));
    }

    #[test]
    fn test_response_provider_cold_start_history_and_temperature_guard() {
        let provider =
            ResponseProvider::new("test-key").with_base_url("https://api.openai.com/v1/responses/");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // System 提示
        session.append_turn(Turn::new(Role::System, vec![Message::text("Be concise.")]));
        // User 问题
        session.append_turn(Turn::user_turn(Message::text("Hello")));
        // Assistant 回复且包含 ToolCall（来自其他 provider 或无 previous_response_id）
        let asst_msg = Message::new(vec![
            ContentPart::text("Calling tool"),
            ContentPart::ToolCall(ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"pwd"}"#.into(),
            }),
        ]);
        session.append_turn(Turn::assistant_turn(vec![asst_msg]));
        // Tool 返回
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult {
                attachments: Vec::new(),
                tool_call_id: "c1".into(),
                content: "/home/project".into(),
                is_error: false,
            })],
        ));

        let options = CompletionOptions {
            model: "gpt-5.6-luna".into(),
            temperature: Some(0.7),
            reasoning_effort: Some("high".into()),
            enable_web_search: true,
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");

        // 验证推理模式下 temperature 被安全抑制
        assert!(body.get("temperature").is_none());
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "concise");

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
    fn test_response_provider_service_tier_injection() {
        let provider = ResponseProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);
        session.append_turn(Turn::user_turn(Message::text("Hello")));

        // annotation.fast: true 时请求体携带 service_tier
        let tiered = CompletionOptions {
            model: "gpt-5.6-luna".into(),
            service_tier: Some("fast".into()),
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &tiered)
            .expect("build body");
        assert_eq!(body["service_tier"], "fast");

        // 缺省时不携带该字段
        let plain = CompletionOptions {
            model: "gpt-5.6-luna".into(),
            ..Default::default()
        };
        let body = provider
            .build_request_body(&session, &[], &plain)
            .expect("build body");
        assert!(body.get("service_tier").is_none());
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
        assert!(
            parsed
                .meta
                .response
                .as_ref()
                .unwrap()
                .output_items
                .iter()
                .any(|item| {
                    matches!(item, ResponseOutputItem::Message { id, .. } if id == "msg_std_100")
                })
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

    #[test]
    fn test_response_provider_store_false_falls_back_to_full_history() {
        let provider = ResponseProvider::new("test-key");
        let mut session = Session::new(PathBuf::from("/tmp"), None);

        // 1. 用户第一轮提问
        let user_msg1 = Message::text("第一轮示例问题");
        session.append_turn(Turn::user_turn(user_msg1));

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
                        "query": "example query",
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
                            "text": "第一轮示例答复。",
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
        assert!(!resp_meta.store);
        // 不记录依赖 store 的 message item_id

        let mut asst_turn = Turn::assistant_turn(parsed.messages);
        asst_turn.meta = parsed.meta;
        assert!(asst_turn.openai_response_id().is_none());
        session.append_turn(asst_turn);

        // 会话无法提取有效 response_id
        assert!(session.latest_openai_response_id().is_none());

        // 3. 用户发起第二轮提问
        let user_msg2 = Message::text("第二轮示例问题");
        session.append_turn(Turn::user_turn(user_msg2));

        let options = CompletionOptions {
            model: "gpt-5.6-terra".into(),
            enable_web_search: true,
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };

        // 4. 构建第二轮请求体：因未留存，必须回退至全量发送 input，且不传 previous_response_id
        let body = provider
            .build_request_body(&session, &[], &options)
            .expect("build body");
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("store").is_none());
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
        assert_eq!(input_items[0]["content"][0]["text"], "第一轮示例问题");
        assert_eq!(input_items[1]["type"], "reasoning");
        assert_eq!(input_items[1]["encrypted_content"], "encrypted-reasoning");
        // 回传 input 的 reasoning item 必须携带 summary 字段（即使为空数组）
        assert_eq!(input_items[1]["summary"], serde_json::json!([]));
        assert_eq!(input_items[2]["type"], "web_search_call");
        assert_eq!(input_items[2]["action"]["query"], "example query");
        assert_eq!(input_items[3]["role"], "assistant");
        assert_eq!(input_items[3]["phase"], "final_answer");
        assert_eq!(input_items[3]["content"][0]["text"], "第一轮示例答复。");
        assert_eq!(input_items[4]["role"], "user");
        assert_eq!(input_items[4]["content"][0]["text"], "第二轮示例问题");
    }

    #[test]
    fn test_response_provider_missing_store_is_not_stored() {
        let provider = ResponseProvider::new("test-key");
        let mock_json_no_store = serde_json::json!({
            "id": "resp_test_1",
            "status": "completed",
            "output": []
        });
        let parsed = provider
            .parse_response(mock_json_no_store)
            .expect("parse response without store");
        let meta = parsed.meta.response.as_ref().unwrap();
        assert!(meta.response_id.is_none());
        assert!(!meta.store);
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

    #[tokio::test]
    async fn test_stream_handler_separates_summary_parts_with_newline() {
        let provider = ResponseProvider::new("test-key");
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
        let mut handler = ResponseStreamHandler::new(&provider, Some(tx));

        // 官方规范流序列：首段 part.added(summary_index=0) → delta → 次段 part.added(1) → delta
        for ev in [
            serde_json::json!({"type": "response.reasoning_summary_part.added", "summary_index": 0}),
            serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "part one"}),
            serde_json::json!({"type": "response.reasoning_summary_part.added", "summary_index": 1}),
            serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "part two"}),
        ] {
            handler.on_event(ev).await.unwrap();
        }

        let mut acc = String::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Thought(text) = event {
                acc.push_str(&text);
            }
        }
        assert_eq!(acc, "part one\n\npart two");

        // summary_index 为规范必填字段，缺失即报协议错误
        let result = handler
            .on_event(serde_json::json!({"type": "response.reasoning_summary_part.added"}))
            .await;
        assert!(
            matches!(result, Err(ProviderError::Protocol(msg)) if msg.contains("Missing 'summary_index'"))
        );
    }

    #[tokio::test]
    async fn test_stream_search_and_citations_arrive_before_completion() {
        let provider = ResponseProvider::new("test-key");
        let (tx, mut rx) = mpsc::channel(16);
        let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
        let search = serde_json::json!({
            "id": "ws_1", "type": "web_search_call", "status": "completed",
            "action": {"type": "search", "query": "Rust release",
                "sources": [{"type": "url", "url": "https://www.rust-lang.org/"}]}
        });
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done", "output_index": 0,
                "sequence_number": 1, "item": search
            }))
            .await
            .unwrap();
        let call = match rx.try_recv().unwrap() {
            AgentEvent::WebSearchCall(call) => call,
            event => panic!("unexpected event: {event:?}"),
        };
        assert_eq!(call.id, "ws_1");
        assert_eq!(call.sources.len(), 1);

        let annotation = serde_json::json!({
            "type": "url_citation", "title": "Rust", "url": "https://www.rust-lang.org/",
            "start_index": 0, "end_index": 4
        });
        handler
            .on_event(serde_json::json!({
                "type": "response.output_text.delta", "item_id": "msg_1",
                "output_index": 1, "content_index": 0, "sequence_number": 2,
                "delta": "Rust"
            }))
            .await
            .unwrap();
        handler
            .on_event(serde_json::json!({
                "type": "response.output_text.annotation.added", "item_id": "msg_1",
                "output_index": 1, "content_index": 0, "annotation_index": 0,
                "sequence_number": 3, "annotation": annotation
            }))
            .await
            .unwrap();
        assert!(
            matches!(rx.try_recv().unwrap(), AgentEvent::Text { text_id, delta }
            if text_id == "msg_1:0" && delta == "Rust")
        );
        let citation: Annotation = serde_json::from_value(annotation.clone()).unwrap();
        assert!(
            matches!(rx.try_recv().unwrap(), AgentEvent::Citation { text_id, annotation }
            if text_id == "msg_1:0" && annotation == citation)
        );
        assert!(handler.completed.is_none());

        handler.on_event(serde_json::json!({
            "type": "response.completed", "sequence_number": 4,
            "response": {"id": "resp_1", "status": "completed", "output": [
                search,
                {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "Rust", "annotations": [annotation]}]}
            ]}
        })).await.unwrap();
        let response = handler.completed.as_ref().unwrap();
        assert_eq!(
            response.messages[0].parts[0],
            ContentPart::WebSearchCall(call)
        );
        assert_eq!(
            response.messages[0].parts[1],
            ContentPart::Text {
                text: "Rust".into(),
                annotations: vec![citation],
            }
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_stream_rejects_invalid_url_citation() {
        let provider = ResponseProvider::new("test-key");
        let mut handler = ResponseStreamHandler::new(&provider, None);
        let result = handler.on_event(serde_json::json!({
            "type": "response.output_text.annotation.added", "item_id": "msg_1",
            "output_index": 0, "content_index": 0, "annotation_index": 0,
            "sequence_number": 1,
            "annotation": {"type": "url_citation", "title": "Rust", "url": "https://www.rust-lang.org/"}
        })).await;
        assert!(
            matches!(result, Err(ProviderError::Protocol(message)) if message.contains("start_index"))
        );
    }

    #[tokio::test]
    async fn test_stream_handler_reports_web_search_call_on_item_done() {
        let provider = ResponseProvider::new("test-key");
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
        let mut handler = ResponseStreamHandler::new(&provider, Some(tx));

        // 每个搜索条目完成时独立上报，保留动作类型。
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": {"type": "search", "query": "example topic"}
                }
            }))
            .await
            .unwrap();
        match rx.try_recv() {
            Ok(AgentEvent::WebSearchCall(call)) => {
                assert_eq!(call.id, "ws_1");
                assert_eq!(call.query, "example topic");
                assert_eq!(call.action, WebSearchAction::Search);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // 打开页面保留目标 URL，message 等其余项不产生搜索事件。
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "web_search_call",
                    "id": "ws_2",
                    "status": "completed",
                    "action": {"type": "open_page", "url": "https://example.com/article"}
                }
            }))
            .await
            .unwrap();
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done",
                "item": {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "Answer", "annotations": []}]}
            }))
            .await
            .unwrap();
        match rx.try_recv() {
            Ok(AgentEvent::WebSearchCall(call)) => {
                assert_eq!(
                    call.action,
                    WebSearchAction::OpenPage {
                        url: "https://example.com/article".into(),
                    }
                );
                assert!(call.query.is_empty());
                assert!(call.sources.is_empty());
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_web_actions_match_stream_final_and_storage() {
        let provider = ResponseProvider::new("test-key");
        let (tx, mut rx) = mpsc::channel(16);
        let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
        let actions = [
            serde_json::json!({"type": "search", "queries": ["query A", "query B"], "sources": [
                {"type": "url", "url": "https://example.com/a"},
                {"type": "url", "url": "https://example.com/b"}
            ]}),
            serde_json::json!({"type": "open_page", "url": "https://example.com/a"}),
            serde_json::json!({"type": "find_in_page", "url": "https://example.com/a", "pattern": "battery"}),
        ];
        let mut items = Vec::new();
        let mut streamed = Vec::new();
        for (index, action) in actions.into_iter().enumerate() {
            let item = serde_json::json!({
                "type": "web_search_call", "id": format!("ws_{index}"), "status": "completed", "action": action
            });
            handler
                .on_event(serde_json::json!({
                    "type": "response.output_item.done", "output_index": index,
                    "sequence_number": index, "item": item
                }))
                .await
                .unwrap();
            let AgentEvent::WebSearchCall(call) = rx.try_recv().unwrap() else {
                panic!("expected search event");
            };
            assert_eq!(call.id, format!("ws_{index}"));
            let encoded = serde_json::to_value(&call).unwrap();
            assert_eq!(
                serde_json::from_value::<WebSearchCall>(encoded).unwrap(),
                call
            );
            streamed.push(ContentPart::WebSearchCall(call));
            items.push(item);
        }
        let ContentPart::WebSearchCall(search) = &streamed[0] else {
            unreachable!()
        };
        assert_eq!(search.action, WebSearchAction::Search);
        assert_eq!(search.query, "query A, query B");
        assert_eq!(search.sources.len(), 2);
        let ContentPart::WebSearchCall(open) = &streamed[1] else {
            unreachable!()
        };
        assert_eq!(
            open.action,
            WebSearchAction::OpenPage {
                url: "https://example.com/a".into()
            }
        );
        assert!(open.sources.is_empty());
        let ContentPart::WebSearchCall(find) = &streamed[2] else {
            unreachable!()
        };
        assert_eq!(
            find.action,
            WebSearchAction::FindInPage {
                url: "https://example.com/a".into(),
                pattern: "battery".into(),
            }
        );
        assert!(find.sources.is_empty());
        let final_response = provider
            .parse_response(serde_json::json!({
                "id": "resp_1", "status": "completed", "output": items,
            }))
            .unwrap();
        assert_eq!(final_response.messages[0].parts, streamed);
    }

    #[tokio::test]
    async fn test_open_page_url_optional_per_spec() {
        let provider = ResponseProvider::new("test-key");
        for action in [
            serde_json::json!({"type": "open_page"}),
            serde_json::json!({"type": "open_page", "url": null}),
        ] {
            let item = serde_json::json!({
                "type": "web_search_call",
                "id": "ws_open",
                "status": "completed",
                "action": action,
            });
            let encoded = serde_json::to_value(
                serde_json::from_value::<ResponseOutputItem>(item.clone()).unwrap(),
            )
            .unwrap();
            assert_eq!(encoded["action"], serde_json::json!({"type": "open_page"}));

            let parsed = provider
                .parse_response(serde_json::json!({
                    "id": "resp_1", "status": "completed", "output": [item.clone()],
                }))
                .unwrap();
            let ContentPart::WebSearchCall(call) = &parsed.messages[0].parts[0] else {
                panic!("expected open_page");
            };
            assert_eq!(
                call.action,
                WebSearchAction::OpenPage { url: String::new() }
            );

            let (tx, mut rx) = mpsc::channel(4);
            let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
            handler
                .on_event(serde_json::json!({
                    "type": "response.output_item.done",
                    "item": item
                }))
                .await
                .unwrap();
            let AgentEvent::WebSearchCall(streamed) = rx.try_recv().unwrap() else {
                panic!("expected search event");
            };
            assert_eq!(streamed.action, call.action);
        }
    }

    #[tokio::test]
    async fn test_missing_web_search_action_projects_empty_search_and_warns() {
        let provider = ResponseProvider::new("test-key");
        let item = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_missing",
            "status": "completed"
        });

        let parsed = provider
            .parse_response(serde_json::json!({
                "id": "resp_1",
                "status": "completed",
                "output": [item.clone()]
            }))
            .unwrap();
        let ContentPart::WebSearchCall(call) = &parsed.messages[0].parts[0] else {
            panic!("expected web search call");
        };
        assert_eq!(call.action, WebSearchAction::Search);
        assert!(call.query.is_empty());
        assert!(call.sources.is_empty());

        let (tx, mut rx) = mpsc::channel(4);
        let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done",
                "item": item
            }))
            .await
            .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentEvent::WebSearchCall(WebSearchCall { query, sources, .. }))
                if query.is_empty() && sources.is_empty()
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentEvent::Notice(message)) if message.contains("omitted action")
        ));
    }

    #[tokio::test]
    async fn test_stream_item_done_sources_survive_completed_aggregation() {
        let provider = ResponseProvider::new("test-key");
        let (tx, mut rx) = mpsc::channel(16);
        let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
        let done_items = [
            serde_json::json!({
                "type": "web_search_call", "id": "ws_0", "status": "completed",
                "action": {"type": "search", "query": "query A", "sources": [
                    {"type": "url", "url": "https://a.example/1"}
                ]}
            }),
            serde_json::json!({
                "type": "web_search_call", "id": "ws_1", "status": "completed",
                "action": {"type": "search", "query": "query B", "sources": [
                    {"type": "url", "url": "https://b.example/2"}
                ]}
            }),
            serde_json::json!({
                "type": "web_search_call", "id": "ws_open", "status": "completed",
                "action": {"type": "open_page", "url": "https://a.example/1"}
            }),
        ];
        for (index, item) in done_items.iter().enumerate() {
            handler
                .on_event(serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": index,
                    "sequence_number": index,
                    "item": item
                }))
                .await
                .unwrap();
            assert!(matches!(
                rx.try_recv().unwrap(),
                AgentEvent::WebSearchCall(_)
            ));
        }

        let union = [
            serde_json::json!({"type": "url", "url": "https://a.example/1"}),
            serde_json::json!({"type": "url", "url": "https://b.example/2"}),
            serde_json::json!({"type": "url", "url": "https://c.example/3"}),
        ];
        handler
            .on_event(serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "status": "completed",
                    "output": [
                        {
                            "type": "web_search_call", "id": "ws_0", "status": "completed",
                            "action": {"type": "search", "query": "query A", "sources": union}
                        },
                        {
                            "type": "web_search_call", "id": "ws_1", "status": "completed",
                            "action": {"type": "search", "query": "query B", "sources": union}
                        },
                        {
                            "type": "web_search_call", "id": "ws_open", "status": "completed",
                            "action": {"type": "open_page", "url": "https://a.example/1"}
                        }
                    ]
                }
            }))
            .await
            .unwrap();

        let parsed = handler.completed.unwrap();
        let parts = &parsed.messages[0].parts;
        let ContentPart::WebSearchCall(first) = &parts[0] else {
            panic!("expected search");
        };
        assert_eq!(
            first
                .sources
                .iter()
                .map(|source| source.url.as_str())
                .collect::<Vec<_>>(),
            ["https://a.example/1"]
        );
        let ContentPart::WebSearchCall(second) = &parts[1] else {
            panic!("expected search");
        };
        assert_eq!(
            second
                .sources
                .iter()
                .map(|source| source.url.as_str())
                .collect::<Vec<_>>(),
            ["https://b.example/2"]
        );
        let ContentPart::WebSearchCall(open) = &parts[2] else {
            panic!("expected open_page");
        };
        assert_eq!(
            open.action,
            WebSearchAction::OpenPage {
                url: "https://a.example/1".into()
            }
        );
        assert!(open.sources.is_empty());

        let ResponseOutputItem::WebSearchCall {
            action: Some(ResponseWebSearchAction::Search { sources, .. }),
            ..
        } = &parsed.meta.response.as_ref().unwrap().output_items[0]
        else {
            panic!("expected native search item");
        };
        assert_eq!(sources.len(), 3);
    }
}
