use serde::{Deserialize, Serialize};

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

// ================== OpenAI Responses API 专有元数据 ==================

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
    /// Responses API 原生 output items，用于无状态会话逐项保真回传。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_items: Vec<ResponseOutputItem>,
    /// 产出该轮时的模型亲和组：回放时与当前请求的亲和组比较，
    /// 不一致则原生条目不可复用（切换模型降维重建）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
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
            output_items: Vec::new(),
            scope: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseOutputItem {
    Reasoning {
        id: String,
        /// Responses API 规范：回传 input 中的 reasoning item 必须携带 summary（可为空数组）
        #[serde(default)]
        summary: Vec<ResponseReasoningSummary>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<ResponseReasoningContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    Message {
        id: String,
        status: String,
        role: ResponseMessageRole,
        content: Vec<ResponseMessageContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: String,
    },
    /// custom（freeform）工具调用：input 为原始文本而非 JSON 参数
    CustomToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        call_id: String,
        name: String,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    WebSearchCall {
        id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<ResponseWebSearchAction>,
    },
    /// 图像生成 server tool 调用（image_generation 工具，官方规范字段一一对应）
    ImageGenerationCall {
        id: String,
        status: String,
        /// 生成的图像 base64（status 为 completed 时返回）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        /// 模型改写后的提示词
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revised_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseReasoningSummary {
    SummaryText { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseReasoningContent {
    ReasoningText { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseMessageRole {
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseMessageContent {
    OutputText {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        annotations: Vec<super::Annotation>,
    },
    Refusal {
        refusal: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseWebSearchAction {
    Search {
        #[serde(skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        queries: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sources: Vec<ResponseWebSearchSource>,
    },
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    FindInPage {
        url: String,
        pattern: String,
    },
}

/// 搜索来源：网页 `url`，或结构化数据 API（如天气）`api`，后者没有 URL
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseWebSearchSource {
    Url { url: String },
    Api { name: String },
}

// ================== Anthropic 专有元数据 ==================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AnthropicTurnMeta {
    /// 缓存统计：原生 Prompt Caching 优势追踪
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// 核心原生优势：Anthropic 原生 assistant content blocks。server tool 结果与引用
    /// 携带服务端加密字段（encrypted_content / encrypted_index），多轮对话必须逐块
    /// 原样回传，缺失或改动即 400；同 Responses output_items 的原生条目链策略
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_blocks: Vec<AnthropicContentBlock>,
    /// 产出该轮时的模型亲和组：回放时与当前请求的亲和组比较，
    /// 不一致则原生块链不可复用（切换模型降维重建）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Anthropic Messages API 原生 assistant content blocks（与官方协议字段一一对应）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<AnthropicCitation>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// 服务端代执行的工具调用（web_search / web_fetch），结果块由服务端直接产出
    ServerToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    WebSearchToolResult {
        tool_use_id: String,
        content: AnthropicSearchResultContent,
    },
    WebFetchToolResult {
        tool_use_id: String,
        content: AnthropicFetchResultContent,
    },
    /// 安全脱敏的思考块：无明文内容，按官方语义原样回传
    RedactedThinking { data: String },
}

/// web_search_result_location 引用：以被引文本与 encrypted_index 定位
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicCitation {
    WebSearchResultLocation {
        url: String,
        title: String,
        /// 被引内容摘录（至多 150 字符）
        cited_text: String,
        /// 服务端加密索引，多轮对话回传必需
        encrypted_index: String,
    },
}

/// web_search_tool_result.content：结果列表或错误对象（HTTP 200 内嵌失败语义）
///
/// 例外于零兜底：部分兼容网关把错误对象包进数组（`[{"type":"web_search_tool_result_error",..}]`），
/// 且回传时同样要求数组；`WrappedError` 保留该形态原样回传。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AnthropicSearchResultContent {
    Results(Vec<AnthropicSearchResult>),
    Error(AnthropicToolResultError),
    WrappedError([AnthropicToolResultError; 1]),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicSearchResult {
    WebSearchResult {
        url: String,
        title: String,
        /// 服务端加密的页面内容快照，多轮回传必须原样保留
        encrypted_content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_age: Option<String>,
    },
}

/// web_fetch_tool_result.content：抓取结果或错误对象
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AnthropicFetchResultContent {
    Result(AnthropicFetchResult),
    Error(AnthropicToolResultError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicFetchResult {
    WebFetchResult {
        url: String,
        /// 抓取页面的正文 document 块（text 或 base64 PDF）
        content: AnthropicDocumentBlock,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retrieved_at: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicDocumentBlock {
    Document {
        source: AnthropicDocumentSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicDocumentSource {
    Text { media_type: String, data: String },
    Base64 { media_type: String, data: String },
}

/// server tool 结果错误对象（type 固定为对应 *_tool_result_error，原样回传）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnthropicToolResultError {
    #[serde(rename = "type")]
    pub block_type: String,
    pub error_code: String,
}

// ================== Google Gemini 专有元数据 ==================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GeminiTurnMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_ratings: Vec<GeminiSafetyRating>,
    /// Gemini 原生代码执行沙盒结果
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution_result: Option<GeminiCodeExecutionResult>,
    /// 原生内容块按接收顺序保存，thoughtSignature 必须随原始 Part 回传。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_parts: Vec<GeminiContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Gemini 原生文本、思考和函数调用块；签名也可能单独出现在空文本块中。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiContentPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GeminiFunctionCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiFunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub args: serde_json::Value,
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
