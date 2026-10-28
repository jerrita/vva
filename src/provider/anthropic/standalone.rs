use std::collections::BTreeMap;
use std::fmt::Write;

use async_trait::async_trait;
use serde::Deserialize;

use super::{AnthropicProvider, DEFAULT_WEB_TOOL_VERSION};
use crate::provider::common::{
    from_value, normalize_endpoint, require_str, require_u64, require_value,
};
use crate::provider::http::{SseEventHandler, post_sse};
use crate::provider::{CompletionOptions, ProviderError};

impl AnthropicProvider {
    pub(crate) async fn standalone_search(
        &self,
        query: &str,
        options: &CompletionOptions,
    ) -> Result<String, ProviderError> {
        let version = options
            .web_tool_version
            .as_deref()
            .unwrap_or(DEFAULT_WEB_TOOL_VERSION);
        let body = serde_json::json!({
            "model": options.model,
            "max_tokens": options.max_tokens.unwrap_or(4096).min(4096),
            "thinking": {"type": "disabled"},
            "system": "You are an assistant for performing a web search tool use.",
            "messages": [{"role": "user", "content": format!("Perform a web search for the query: {query}")}],
            "tools": [{"type": format!("web_search_{version}"), "name": "web_search", "allowed_callers": ["direct"], "max_uses": 8}],
            "tool_choice": {"type": "tool", "name": "web_search"},
            "stream": true
        });
        let mut handler = SearchStream::default();
        post_sse(
            &normalize_endpoint(&self.base_url, "messages"),
            self.auth_headers()?,
            &body,
            &mut handler,
        )
        .await?;
        handler.finish(query)
    }
}

// 独立搜索只提取展示所需字段，不建立服务端调用链，也不回传原生搜索块。
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SearchBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    WebSearchToolResult {
        content: SearchContent,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SearchContent {
    Results(Vec<SearchResult>),
    Error(SearchError),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SearchResult {
    WebSearchResult { url: String, title: Option<String> },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SearchError {
    WebSearchToolResultError { error_code: String },
}

#[derive(Default)]
struct SearchStream {
    blocks: BTreeMap<u64, SearchBlock>,
    stopped: bool,
}

impl SearchStream {
    fn finish(self, query: &str) -> Result<String, ProviderError> {
        if !self.stopped {
            return Err(ProviderError::Protocol(
                "Standalone search stream ended without message_stop".into(),
            ));
        }
        let mut output = format!("Web search results for query: {query}\n\n");
        let mut searched = false;
        let mut results = 0;
        for block in self.blocks.into_values() {
            match block {
                SearchBlock::Text { text } if !text.is_empty() => {
                    writeln!(output, "{text}\n").unwrap();
                }
                SearchBlock::WebSearchToolResult { content } => match content {
                    SearchContent::Results(sources) => {
                        searched = true;
                        for SearchResult::WebSearchResult { url, title } in sources {
                            results += 1;
                            if let Some(title) = title {
                                writeln!(output, "{results}. {title}\n{url}\n").unwrap();
                            } else {
                                writeln!(output, "{results}. {url}\n").unwrap();
                            }
                        }
                    }
                    SearchContent::Error(SearchError::WebSearchToolResultError { error_code }) => {
                        return Err(ProviderError::Protocol(format!(
                            "Standalone web search failed: {error_code}"
                        )));
                    }
                },
                _ => {}
            }
        }
        if !searched {
            return Err(ProviderError::Protocol(
                "Standalone search returned no web_search_tool_result".into(),
            ));
        }
        if results == 0 {
            output.push_str("No results found.\n");
        }
        Ok(output.trim_end().to_string())
    }
}

#[async_trait]
impl SseEventHandler for SearchStream {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError> {
        match require_str(&json, "type")? {
            "content_block_start" => {
                let index = require_u64(&json, "index")?;
                let block = from_value(
                    require_value(&json, "content_block")?,
                    "standalone search block",
                )?;
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let delta = require_value(&json, "delta")?;
                if require_str(delta, "type")? == "text_delta" {
                    let index = require_u64(&json, "index")?;
                    let text = require_str(delta, "text")?;
                    let Some(SearchBlock::Text { text: output }) = self.blocks.get_mut(&index)
                    else {
                        return Err(ProviderError::Protocol(format!(
                            "Standalone search text_delta on non-text block {index}"
                        )));
                    };
                    output.push_str(text);
                }
            }
            "message_stop" => self.stopped = true,
            "error" => {
                return Err(ProviderError::Protocol(format!(
                    "Standalone search stream error: {}",
                    require_str(require_value(&json, "error")?, "message")?
                )));
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn standalone_search_reports_errors_and_incomplete_streams() {
        let mut interrupted = SearchStream::default();
        interrupted.on_event(json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "web_search_tool_result", "tool_use_id": "result_1", "content": []}
        })).await.unwrap();
        assert!(
            interrupted
                .finish("example")
                .unwrap_err()
                .to_string()
                .contains("message_stop")
        );

        let mut empty = SearchStream::default();
        empty.on_event(json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "web_search_tool_result", "tool_use_id": "result_1", "content": []}
        })).await.unwrap();
        empty
            .on_event(json!({"type": "message_stop"}))
            .await
            .unwrap();
        assert!(
            empty
                .finish("example")
                .unwrap()
                .contains("No results found")
        );

        let mut missing = SearchStream::default();
        missing
            .on_event(json!({"type": "message_stop"}))
            .await
            .unwrap();
        assert!(
            missing
                .finish("example")
                .unwrap_err()
                .to_string()
                .contains("no web_search_tool_result")
        );

        let mut failed = SearchStream::default();
        failed.on_event(json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "web_search_tool_result", "tool_use_id": "result_1",
                "content": {"type": "web_search_tool_result_error", "error_code": "unavailable"}}
        })).await.unwrap();
        failed
            .on_event(json!({"type": "message_stop"}))
            .await
            .unwrap();
        assert!(
            failed
                .finish("example")
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );

        let mut malformed = SearchStream::default();
        assert!(
            malformed
                .on_event(json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": {"type": "web_search_tool_result", "content": [
                        {"type": "web_search_result", "title": "Missing URL"}
                    ]}
                }))
                .await
                .is_err()
        );
        assert!(
            malformed
                .on_event(json!({
                    "type": "error", "error": {"type": "overloaded_error", "message": "overloaded"}
                }))
                .await
                .unwrap_err()
                .to_string()
                .contains("overloaded")
        );
    }
}
