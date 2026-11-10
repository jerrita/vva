use serde::{Deserialize, Serialize};

use super::tool::{ToolCall, ToolResult};

/// 对齐 OpenAI Responses 与多模态架构的统一内容块
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// 富文本（支持内嵌注脚引用 annotations）
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        annotations: Vec<Annotation>,
    },
    /// 模型深度思考过程
    Reasoning {
        content: String,
        /// Anthropic thinking signature 或 Gemini thought token
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// 图像模态
    Image {
        source: MediaSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    /// 文件模态（当前原生支持 PDF）
    File {
        source: MediaSource,
        mime_type: String,
        filename: String,
    },
    /// 用户以 `@path` 引用的本地文本文件内容（提交时由 harness 展开，原文保留在提示词中）
    FileRef {
        path: String,
        content: String,
        #[serde(default)]
        truncated: bool,
    },
    /// 通用工具调用
    ToolCall(ToolCall),
    /// 通用工具执行结果
    ToolResult(ToolResult),
    /// 参考 Responses API 的 Web Search 原生调用模型
    WebSearchCall(WebSearchCall),
    /// 参考 Responses API 的 image_generation_call 原生调用模型
    ImageGenerationCall(ImageGenerationCall),
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            annotations: Vec::new(),
        }
    }

    pub fn reasoning(content: impl Into<String>, signature: Option<String>) -> Self {
        Self::Reasoning {
            content: content.into(),
            signature,
        }
    }

    pub fn image_url(url: impl Into<String>) -> Self {
        Self::Image {
            source: MediaSource::Url { url: url.into() },
            mime_type: None,
        }
    }
}

/// FileRef 部件在各供应商请求中的统一文本投影：`@path` 头 + 全文，截断时追加标记
pub fn format_file_ref(path: &str, content: &str, truncated: bool) -> String {
    let mut out = format!("@{path}\n{content}");
    if truncated {
        out.push_str("\n[content truncated]");
    }
    out
}

/// 媒体资源来源定位
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSource {
    Url { url: String },
    Base64 { data: String, mime_type: String },
}

/// 参考 OpenAI Responses API 规范的注脚与引用系统
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Annotation {
    UrlCitation {
        url: String,
        title: String,
        start_index: usize,
        end_index: usize,
    },
    /// Anthropic web search 引用：无字符偏移，以被引文本摘录定位
    WebSearchResultLocation {
        url: String,
        title: String,
        cited_text: String,
    },
    /// Responses API 规范定义的文件引用注脚（当前流式路径未产出，保留为协议词汇）
    FileCitation {
        file_id: String,
        filename: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        quote: Option<String>,
        start_index: usize,
        end_index: usize,
    },
}

impl Annotation {
    /// URL 类引用的 (标题, 链接)；文件引用无此二者
    pub fn title_url(&self) -> Option<(&str, &str)> {
        match self {
            Self::UrlCitation { title, url, .. }
            | Self::WebSearchResultLocation { title, url, .. } => Some((title, url)),
            Self::FileCitation { .. } => None,
        }
    }
}

/// 参考 OpenAI Responses API 的 web_search_call 结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchCall {
    pub id: String,
    pub query: String,
    /// None 表示端点未下发动作（流式中途或兼容端点收尾仍缺）；
    /// 旧落盘条目缺少该字段时按 Some(Search) 兼容读取，新条目以 null 往返。
    #[serde(default = "legacy_search_action")]
    pub action: Option<WebSearchAction>,
    pub status: SearchStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<WebSearchSource>,
}

impl WebSearchCall {
    pub fn completed(
        id: impl Into<String>,
        query: impl Into<String>,
        sources: Vec<WebSearchSource>,
    ) -> Self {
        Self {
            id: id.into(),
            query: query.into(),
            action: Some(WebSearchAction::Search),
            status: SearchStatus::Completed,
            sources,
        }
    }

    /// 历史重建时的结果文本投影：编号 title/url 列表，供客户端工具形态的
    /// function_call_output / tool_result 携带搜索上下文跨亲和组流转
    pub fn result_text(&self) -> String {
        match &self.action {
            Some(WebSearchAction::Search) => {
                if self.sources.is_empty() {
                    return format!("No results found for \"{}\".", self.query);
                }
                let mut text = String::new();
                for (index, source) in self.sources.iter().enumerate() {
                    text.push_str(&format!("{}. ", index + 1));
                    if !source.title.is_empty() {
                        text.push_str(&format!("{}\n", source.title));
                    }
                    text.push_str(&format!("{}\n\n", source.url));
                }
                text
            }
            Some(WebSearchAction::OpenPage { url }) => format!("Fetched {url}."),
            Some(WebSearchAction::FindInPage { url, pattern }) => {
                format!("Found \"{pattern}\" in {url}.")
            }
            None => "No search action was reported.".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    Search,
    OpenPage { url: String },
    FindInPage { url: String, pattern: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SearchStatus {
    Pending,
    Searching,
    Completed,
    Failed,
}

/// 旧落盘条目缺少 action 字段时的兼容缺省：按既有展示语义回读为 Search
fn legacy_search_action() -> Option<WebSearchAction> {
    Some(WebSearchAction::Search)
}

/// 参考 OpenAI Responses API 规范的 image_generation_call.status 三值枚举
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenerationStatus {
    InProgress,
    Completed,
    Incomplete,
}

impl ImageGenerationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
        }
    }
}

/// 参考 OpenAI Responses API 的 image_generation_call 结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageGenerationCall {
    pub id: String,
    pub status: ImageGenerationStatus,
    /// status 为 completed 时的 base64 图像结果，mime 按官方 output_format（缺省 png）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<MediaSource>,
    /// 模型改写后的提示词
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchSource {
    pub url: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

impl WebSearchSource {
    pub fn new(url: impl Into<String>, title: impl Into<String>, snippet: Option<String>) -> Self {
        Self {
            url: url.into(),
            title: title.into(),
            snippet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_search_without_action_remains_readable() {
        let call: WebSearchCall = serde_json::from_value(serde_json::json!({
            "id": "ws_old", "query": "query", "status": "completed",
            "sources": [{"url": "https://example.com", "title": "Example"}]
        }))
        .unwrap();
        assert_eq!(call.action, Some(WebSearchAction::Search));
        assert_eq!(call.query, "query");
        assert_eq!(call.sources.len(), 1);
    }

    #[test]
    fn actionless_search_roundtrips_as_null() {
        let call = WebSearchCall {
            id: "ws_new".into(),
            query: String::new(),
            action: None,
            status: SearchStatus::Completed,
            sources: Vec::new(),
        };
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["action"], serde_json::Value::Null);
        let back: WebSearchCall = serde_json::from_value(json).unwrap();
        assert_eq!(back.action, None);
    }
}
