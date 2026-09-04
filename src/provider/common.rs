//! 跨供应商共享的协议语义辅助：事件转发、严格字段读取与相邻消息合并。
//!
//! 本模块只承载与厂商无关的机械逻辑（各适配器逐行同构的部分），
//! 不包含任何字段别名猜测或协议兜底——厂商专属语义留在各自适配器中。

use tokio::sync::mpsc;

use super::ProviderError;
use crate::event::AgentEvent;
use crate::model::ToolDefinition;

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

    pub async fn thought(&self, delta: &str) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(AgentEvent::Thought(delta.to_string())).await;
        }
    }

    pub async fn text(&self, delta: &str) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(AgentEvent::Text(delta.to_string())).await;
        }
    }

    pub async fn citation(&self, title: &str, url: &str) {
        if let Some(tx) = &self.tx {
            let _ = tx
                .send(AgentEvent::Citation {
                    title: title.to_string(),
                    url: url.to_string(),
                })
                .await;
        }
    }
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

/// 构建函数工具声明的公共字段（name / description / 参数表）。
/// 参数表字段名因协议而异（Responses 与 Gemini 为 `parameters`，Anthropic 为 `input_schema`），
/// 由调用方传入；协议专属的其余字段（如 Responses 的 `"type": "function"`）由调用方补齐。
pub fn function_tool_def(tool: &ToolDefinition, parameters_key: &str) -> serde_json::Value {
    let mut def = serde_json::json!({
        "name": tool.name,
        "description": tool.description,
    });
    def[parameters_key] = tool.parameters.clone();
    def
}
