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

/// 参考 OpenAI Responses API 的 web_search_call 结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchCall {
    pub id: String,
    pub query: String,
    /// 已持久化的旧条目缺少动作字段，沿用其搜索展示。
    #[serde(default)]
    pub action: WebSearchAction,
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
            action: WebSearchAction::Search,
            status: SearchStatus::Completed,
            sources,
        }
    }

    /// 历史重建时的结果文本投影：编号 title/url 列表，供客户端工具形态的
    /// function_call_output / tool_result 携带搜索上下文跨亲和组流转
    pub fn result_text(&self) -> String {
        match &self.action {
            WebSearchAction::Search => {
                if self.sources.is_empty() {
                    return format!("No results found for \"{}\".", self.query);
                }
                let mut text = String::new();
                for (index, source) in self.sources.iter().enumerate() {
                    if source.title.is_empty() {
                        text.push_str(&format!("{}. {}\n", index + 1, source.url));
                    } else {
                        text.push_str(&format!(
                            "{}. {}\n{}\n",
                            index + 1,
                            source.title,
                            source.url
                        ));
                    }
                    text.push('\n');
                }
                text
            }
            WebSearchAction::OpenPage { url } => format!("Fetched {url}."),
            WebSearchAction::FindInPage { url, pattern } => {
                format!("Found \"{pattern}\" in {url}.")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    #[default]
    Search,
    OpenPage {
        url: String,
    },
    FindInPage {
        url: String,
        pattern: String,
    },
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
        assert_eq!(call.action, WebSearchAction::Search);
        assert_eq!(call.query, "query");
        assert_eq!(call.sources.len(), 1);
    }
}
