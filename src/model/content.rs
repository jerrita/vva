use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    /// 结构化文档模态 (PDF, Markdown, etc.)
    Document {
        source: MediaSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
    /// 音频模态
    Audio {
        source: MediaSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    /// 视频模态
    Video {
        source: MediaSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    /// 通用工具调用
    ToolCall(ToolCall),
    /// 通用工具执行结果
    ToolResult(ToolResult),
    /// 参考 Responses API 的 Web Search 原生调用模型
    WebSearchCall(WebSearchCall),
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            annotations: Vec::new(),
        }
    }

    pub fn text_with_annotations(text: impl Into<String>, annotations: Vec<Annotation>) -> Self {
        Self::Text {
            text: text.into(),
            annotations,
        }
    }

    pub fn reasoning(content: impl Into<String>, signature: Option<String>) -> Self {
        Self::Reasoning {
            content: content.into(),
            signature,
        }
    }

    pub fn image_url(url: impl Into<String>, detail: Option<ImageDetail>) -> Self {
        Self::Image {
            source: MediaSource::Url { url: url.into() },
            mime_type: None,
            detail,
        }
    }

    pub fn image_path(path: impl Into<PathBuf>, mime_type: Option<String>) -> Self {
        Self::Image {
            source: MediaSource::Path { path: path.into() },
            mime_type,
            detail: None,
        }
    }

    pub fn document_path(
        path: impl Into<PathBuf>,
        filename: Option<String>,
        mime_type: Option<String>,
    ) -> Self {
        Self::Document {
            source: MediaSource::Path { path: path.into() },
            mime_type,
            filename,
        }
    }
}

/// 媒体资源来源定位
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSource {
    Url { url: String },
    Base64 { data: String, mime_type: String },
    Path { path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
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
    FileCitation {
        file_id: String,
        filename: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        quote: Option<String>,
        start_index: usize,
        end_index: usize,
    },
}

/// 参考 OpenAI Responses API 的 web_search_call 结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchCall {
    pub id: String,
    pub query: String,
    pub status: SearchStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<WebSearchSource>,
}

impl WebSearchCall {
    pub fn new(id: impl Into<String>, query: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            query: query.into(),
            status: SearchStatus::Searching,
            sources: Vec::new(),
        }
    }

    pub fn completed(
        id: impl Into<String>,
        query: impl Into<String>,
        sources: Vec<WebSearchSource>,
    ) -> Self {
        Self {
            id: id.into(),
            query: query.into(),
            status: SearchStatus::Completed,
            sources,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SearchStatus {
    Pending,
    Searching,
    Completed,
    Failed,
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
