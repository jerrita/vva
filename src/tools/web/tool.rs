//! web_search / web_fetch 客户端工具：与原生 server tool 同名，
//! 仅在模型原生能力缺位且配置了外置 provider 时注册（原生在位时名称由 reserve 保留）。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::{WebFetchProvider, WebSearchProvider};
use crate::tools::Tool;
use crate::tools::parse_args;

const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS_CAP: usize = 20;

pub struct WebSearchTool {
    provider: Arc<dyn WebSearchProvider>,
}

impl WebSearchTool {
    pub fn new(provider: Arc<dyn WebSearchProvider>) -> Self {
        Self { provider }
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    max_results: Option<usize>,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        r#"Search the web for current information beyond your knowledge or the workspace. Returns a numbered list of results with title, URL, and a relevant content snippet, ordered by relevance. Use it for recent events, library versions, documentation, or facts you are unsure about. Follow up promising results with web_fetch to read the full page.
Example arguments: {"query":"rust tokio select macro example","max_results":5}."#
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The search query." },
                    "max_results": { "type": "integer", "description": "Maximum number of results to return, 1-20. Defaults to 5." }
                },
                "required": ["query"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: SearchArgs = parse_args(arguments, self.name())?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err("query must not be empty".to_string());
        }
        let max_results = args
            .max_results
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, MAX_RESULTS_CAP);
        let results = self.provider.search(query, max_results).await?;
        if results.is_empty() {
            return Ok(format!("No results found for \"{query}\"."));
        }
        let mut output = String::new();
        for (index, result) in results.iter().enumerate() {
            if result.title.is_empty() {
                output.push_str(&format!("{}. {}\n", index + 1, result.url));
            } else {
                output.push_str(&format!(
                    "{}. {}\n{}\n",
                    index + 1,
                    result.title,
                    result.url
                ));
            }
            if !result.snippet.is_empty() {
                output.push_str(&result.snippet);
                output.push('\n');
            }
            output.push('\n');
        }
        Ok(output)
    }
}

pub struct WebFetchTool {
    provider: Arc<dyn WebFetchProvider>,
}

impl WebFetchTool {
    pub fn new(provider: Arc<dyn WebFetchProvider>) -> Self {
        Self { provider }
    }
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        r#"Fetch a web page by URL and return its main textual content as Markdown, with navigation, ads, and other page chrome removed. Use it to read a full page after a web_search hit, or directly when you already know the URL. Only http(s) URLs are supported; non-HTML resources and JavaScript-only pages may fail or return little content.
Example arguments: {"url":"https://docs.rs/serde/latest/serde/"}."#
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The absolute http(s) URL of the page to fetch." }
                },
                "required": ["url"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: FetchArgs = parse_args(arguments, self.name())?;
        let url = args.url.trim();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!("url must start with http:// or https://: {url}"));
        }
        let content = self.provider.fetch(url).await?;
        if content.title.is_empty() {
            Ok(content.content)
        } else {
            Ok(format!("# {}\n\n{}", content.title, content.content))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSearch;

    #[async_trait]
    impl WebSearchProvider for StubSearch {
        fn name(&self) -> &'static str {
            "stub"
        }

        async fn search(
            &self,
            _query: &str,
            max_results: usize,
        ) -> Result<Vec<super::super::WebSearchResult>, String> {
            Ok((0..max_results.min(2))
                .map(|index| super::super::WebSearchResult {
                    title: format!("Title {index}"),
                    url: format!("https://example.com/{index}"),
                    snippet: format!("snippet {index}"),
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn formats_numbered_results() {
        let tool = WebSearchTool::new(Arc::new(StubSearch));
        let output = tool
            .execute(r#"{"query":"rust async","max_results":5}"#)
            .await
            .unwrap();

        assert!(
            output.starts_with("1. Title 0\nhttps://example.com/0\n"),
            "{output}"
        );
        assert!(output.contains("2. Title 1"));
    }

    #[tokio::test]
    async fn rejects_empty_query() {
        let tool = WebSearchTool::new(Arc::new(StubSearch));
        let error = tool.execute(r#"{"query":"  "}"#).await.unwrap_err();

        assert_eq!(error, "query must not be empty");
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http_url() {
        struct StubFetch;

        #[async_trait]
        impl WebFetchProvider for StubFetch {
            fn name(&self) -> &'static str {
                "stub"
            }

            async fn fetch(&self, _url: &str) -> Result<super::super::WebFetchContent, String> {
                unreachable!()
            }
        }

        let tool = WebFetchTool::new(Arc::new(StubFetch));
        let error = tool
            .execute(r#"{"url":"ftp://example.com/file"}"#)
            .await
            .unwrap_err();

        assert!(error.contains("http"), "unexpected: {error}");
    }

    #[test]
    fn parameter_schemas_match_registry_validation() {
        let search = WebSearchTool::new(Arc::new(StubSearch));
        let call = crate::model::ToolCall::new("c1", "web_search", r#"{"query":"q"}"#);
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Arc::new(search) as Arc<dyn Tool>);
        assert!(registry.validate_call(&call).is_ok());
    }

    #[tokio::test]
    async fn oversized_max_results_is_clamped_not_rejected() {
        let search = WebSearchTool::new(Arc::new(StubSearch));
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Arc::new(search) as Arc<dyn Tool>);
        let result = registry
            .execute_call(&crate::model::ToolCall::new(
                "c2",
                "web_search",
                r#"{"query":"q","max_results":99}"#,
            ))
            .await;

        assert!(!result.is_error);
        assert!(result.content.starts_with("1."), "{}", result.content);
    }
}
