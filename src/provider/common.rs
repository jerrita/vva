//! 跨供应商共享的协议语义辅助：事件转发、严格字段读取与相邻消息合并。
//!
//! 本模块只承载与厂商无关的机械逻辑（各适配器逐行同构的部分），
//! 不包含任何字段别名猜测或协议兜底——厂商专属语义留在各自适配器中。

use reqwest::header::HeaderValue;
use tokio::sync::mpsc;

use super::{CompletionOptions, ProviderError};
use crate::event::AgentEvent;
use crate::model::{
    Annotation, ImageGenerationCall, ToolCall, ToolCallKind, ToolDefinition, ToolInput,
    WebSearchCall,
};

/// 增量事件转发器：统一封装 `Option<Sender>` 的转发样板。
/// 全部走 `send().await` 以保留背压语义（通道满时等待而非丢弃事件）。
#[derive(Default)]
pub struct EventForwarder {
    tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EventForwarder {
    pub fn new(tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { tx }
    }

    /// 有订阅者时按背压语义转发（通道关闭视为无订阅者，忽略）
    pub async fn send(&self, event: AgentEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(event).await;
        }
    }

    pub async fn thought(&self, delta: &str) {
        self.send(AgentEvent::Thought(delta.to_string())).await;
    }

    pub async fn text(&self, text_id: &str, delta: &str) {
        self.send(AgentEvent::Text {
            text_id: text_id.to_string(),
            delta: delta.to_string(),
        })
        .await;
    }

    pub async fn citation(&self, text_id: &str, annotation: Annotation) {
        self.send(AgentEvent::Citation {
            text_id: text_id.to_string(),
            annotation,
        })
        .await;
    }

    pub async fn web_search_call(&self, call: WebSearchCall) {
        self.send(AgentEvent::WebSearchCall(call)).await;
    }

    pub async fn image_generation_call(&self, call: ImageGenerationCall) {
        self.send(AgentEvent::ImageGenerationCall(call)).await;
    }

    pub async fn notice(&self, text: impl Into<String>) {
        self.send(AgentEvent::Notice(text.into())).await;
    }
}

/// 规范化请求端点：base_url 无论是否携带 `/{suffix}` 后缀，均收敛到唯一端点
pub fn normalize_endpoint(base_url: &str, suffix: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix(&format!("/{suffix}")).unwrap_or(base);
    format!("{base}/{suffix}")
}

/// API key 转 Header 值：含非法字符即协议错误
pub fn header_value(value: &str) -> Result<HeaderValue, ProviderError> {
    HeaderValue::from_str(value).map_err(|e| ProviderError::Protocol(e.to_string()))
}

/// 严格读取必填 u64 字段：缺失或类型不符直接报协议错误
pub fn require_u64(json: &serde_json::Value, field: &str) -> Result<u64, ProviderError> {
    json[field]
        .as_u64()
        .ok_or_else(|| ProviderError::Protocol(format!("Missing '{field}'")))
}

/// 严格读取必填字符串字段：缺失或类型不符直接报协议错误
pub fn require_str<'a>(json: &'a serde_json::Value, field: &str) -> Result<&'a str, ProviderError> {
    json[field]
        .as_str()
        .ok_or_else(|| ProviderError::Protocol(format!("Missing '{field}'")))
}

/// 严格读取必填布尔字段：缺失或类型不符直接报协议错误
pub fn require_bool(json: &serde_json::Value, field: &str) -> Result<bool, ProviderError> {
    json[field]
        .as_bool()
        .ok_or_else(|| ProviderError::Protocol(format!("Missing '{field}'")))
}

/// 严格读取必填子结构（对象 / 数组等）：缺失即报协议错误
pub fn require_value<'a>(
    json: &'a serde_json::Value,
    field: &str,
) -> Result<&'a serde_json::Value, ProviderError> {
    match json.get(field) {
        Some(value) if !value.is_null() => Ok(value),
        _ => Err(ProviderError::Protocol(format!("Missing '{field}'"))),
    }
}

/// 严格读取必填数组字段：缺失或类型不符直接报协议错误
pub fn require_array<'a>(
    json: &'a serde_json::Value,
    field: &str,
) -> Result<&'a Vec<serde_json::Value>, ProviderError> {
    json[field]
        .as_array()
        .ok_or_else(|| ProviderError::Protocol(format!("Missing '{field}'")))
}

/// 强类型反序列化协议子结构：失败即报协议错误（含结构名与 serde 原因），
/// 统一各适配器重复的 `from_value(..).map_err(..)` 样板
pub fn from_value<T: serde::de::DeserializeOwned>(
    json: &serde_json::Value,
    what: &str,
) -> Result<T, ProviderError> {
    T::deserialize(json).map_err(|e| ProviderError::Protocol(format!("Invalid {what}: {e}")))
}

/// 相邻同角色消息合并器。
/// Anthropic（user/assistant 严格交替）与 Gemini（user/model 角色）都要求
/// 将连续的同角色内容块聚合进同一条消息，仅 `content` / `parts` 字段名不同。
pub struct RoleBlockMerger {
    blocks_key: &'static str,
    messages: Vec<serde_json::Value>,
}

impl RoleBlockMerger {
    pub fn new(blocks_key: &'static str) -> Self {
        Self {
            blocks_key,
            messages: Vec::new(),
        }
    }

    /// 追加一组内容块：与上一条消息同角色时合并进去，空块直接丢弃
    pub fn push(&mut self, role: &str, blocks: Vec<serde_json::Value>) {
        if blocks.is_empty() {
            return;
        }
        if let Some(last) = self.messages.last_mut()
            && last["role"] == role
            && let Some(arr) = last[self.blocks_key].as_array_mut()
        {
            arr.extend(blocks);
            return;
        }
        self.messages.push(serde_json::json!({
            "role": role,
            self.blocks_key: blocks,
        }));
    }

    pub fn into_messages(self) -> Vec<serde_json::Value> {
        self.messages
    }
}

/// 跨亲和组重建时，领域 WebSearchCall 在当前请求工具面上的投影形态
pub(crate) enum WebCallSurface {
    /// 原生 server tool 在位（名称 + 入参），按各协议的历史块形态回传
    Native(&'static str, serde_json::Value),
    /// 仅外置客户端函数在位（名称 + 入参），合成 function 调用与结果对
    Client(&'static str, serde_json::Value),
}

/// 按当前工具面为搜索调用选择投影：原生 server tool 在位优先，
/// 其次外置客户端函数，皆无则 None（该轮丢弃）。
/// 搜索调用是不绑定签发者的纯数据，可跨亲和组流转；决策只看工具面。
/// OpenPage / FindInPage：有独立 web_fetch 时走该工具；否则随 web_search（Response open_page）。
pub(crate) fn web_call_surface(
    call: &WebSearchCall,
    options: &CompletionOptions,
    tools: &[ToolDefinition],
    server_tools: &[&'static str],
) -> Option<WebCallSurface> {
    let (native_name, client_name, input) = match &call.action {
        Some(crate::model::WebSearchAction::Search) => (
            "web_search",
            "web_search",
            serde_json::json!({ "query": call.query }),
        ),
        Some(crate::model::WebSearchAction::OpenPage { url })
        | Some(crate::model::WebSearchAction::FindInPage { url, .. }) => {
            let native_name = if server_tools.contains(&"web_fetch") {
                "web_fetch"
            } else {
                "web_search"
            };
            (native_name, "web_fetch", serde_json::json!({ "url": url }))
        }
        // 动作未下发的条目无法在任何协议形态下如实回传，跨亲和组重建时丢弃；
        // 同源 Responses 回放走 TurnMeta.output_items 原样条目，不受影响。
        None => return None,
    };
    let native_enabled = match native_name {
        "web_search" => options.enable_web_search,
        _ => options.enable_web_fetch,
    };
    if native_enabled && server_tools.contains(&native_name) {
        return Some(WebCallSurface::Native(native_name, input));
    }
    let client_declared = tools.iter().any(|tool| tool.name == client_name);
    client_declared.then_some(WebCallSurface::Client(client_name, input))
}

#[cfg(test)]
pub(crate) fn media_tool_session() -> crate::model::Session {
    use crate::model::*;
    let mut result = ToolResult::success("read_media", "Read sample.png");
    result.attachments.push(MediaContent {
        kind: MediaKind::Image,
        mime_type: "image/png".into(),
        filename: "sample.png".into(),
        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=".into(),
    });
    let mut session = Session::new(".", None);
    session.append_turn(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::ToolCall(ToolCall::new(
            "read_media",
            "Read",
            r#"{"path":"sample.png"}"#,
        )),
    ])]));
    session.append_turn(Turn::new(Role::Tool, vec![Message::tool_result(result)]));
    session
}

/// Freeform 工具投影为 function 时的单字段入参描述
pub(crate) const FREEFORM_INPUT_DESCRIPTION: &str =
    "Raw tool input text, exactly as the tool description specifies.";

/// 该名称在本次请求的工具中是否为 Freeform 工具
pub(crate) fn is_freeform_tool(tools: &[ToolDefinition], name: &str) -> bool {
    tools
        .iter()
        .any(|t| t.name == name && matches!(t.input, ToolInput::Freeform { .. }))
}

/// 从 function 调用参数中取回 Freeform 工具的原始输入：必须恰为仅含字符串 `input` 的对象
pub(crate) fn freeform_call_input(
    name: &str,
    args: &serde_json::Value,
) -> Result<String, ProviderError> {
    args.as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.get("input"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            ProviderError::Protocol(format!(
                "Freeform tool call '{name}' arguments must be an object with a single string 'input'"
            ))
        })
}

/// 历史调用回放为协议参数对象：Custom 调用重新包装为 `{"input": 原文}`，Function 调用解析 JSON 文本
pub(crate) fn replay_call_args(call: &ToolCall) -> Result<serde_json::Value, ProviderError> {
    match call.kind {
        ToolCallKind::Custom => Ok(serde_json::json!({"input": call.arguments})),
        ToolCallKind::Function => serde_json::from_str(&call.arguments).map_err(|e| {
            ProviderError::Protocol(format!(
                "Invalid tool call '{}' arguments JSON: {e}",
                call.name
            ))
        }),
    }
}

/// 工具定义的 function 参数 schema：Freeform 工具在不支持 custom tool 的协议上投影为单字段 `input`
pub(crate) fn function_parameters(input: &ToolInput) -> serde_json::Value {
    match input {
        ToolInput::Json(schema) => schema.clone(),
        ToolInput::Freeform { .. } => serde_json::json!({
            "type": "object",
            "properties": {
                "input": {"type": "string", "description": FREEFORM_INPUT_DESCRIPTION}
            },
            "required": ["input"],
            "additionalProperties": false
        }),
    }
}
