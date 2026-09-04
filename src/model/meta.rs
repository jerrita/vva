use serde::{Deserialize, Serialize};

/// 供应商专有的全局会话级元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseSessionMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<AnthropicSessionMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<GeminiSessionMeta>,
}

/// 供应商专有的交互轮次级元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseTurnMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<AnthropicTurnMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<GeminiTurnMeta>,
}

/// 供应商专有的消息级元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MessageMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseMessageMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<AnthropicMessageMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<GeminiMessageMeta>,
}

// ================== OpenAI Responses API 专有元数据 ==================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResponseSessionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponseTurnMeta {
    /// 核心原生优势：服务端的链式状态追踪 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub store: bool,
}

impl ResponseTurnMeta {
    pub fn new(response_id: impl Into<String>) -> Self {
        Self {
            response_id: Some(response_id.into()),
            previous_response_id: None,
            status: "completed".to_string(),
            service_tier: None,
            reasoning_effort: None,
            store: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResponseMessageMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
}

// ================== Anthropic 专有元数据 ==================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AnthropicSessionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AnthropicTurnMeta {
    /// 缓存统计：原生 Prompt Caching 优势追踪
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AnthropicMessageMeta {
    /// 缓存断点设置 (cache_control: {"type": "ephemeral"})
    #[serde(default)]
    pub ephemeral_cache: bool,
}

// ================== Google Gemini 专有元数据 ==================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GeminiSessionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GeminiTurnMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_ratings: Vec<GeminiSafetyRating>,
    /// Gemini 原生代码执行沙盒结果
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution_result: Option<GeminiCodeExecutionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GeminiMessageMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiSafetyRating {
    pub category: String,
    pub probability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiCodeExecutionResult {
    pub outcome: String,
    pub output: String,
}

