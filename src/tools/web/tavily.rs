//! Tavily API provider：search（`POST /search`）与 fetch（`POST /extract`），
//! Bearer 认证（`Authorization: Bearer <api_key>`）。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{WebFetchContent, WebSearchResult};
use crate::provider::shared_client;

const SEARCH_URL: &str = "https://api.tavily.com/search";
const EXTRACT_URL: &str = "https://api.tavily.com/extract";

async fn post_json<T: serde::de::DeserializeOwned>(
    url: &str,
    api_key: &str,
    body: &impl Serialize,
    provider: &str,
) -> Result<T, String> {
    let response = shared_client()
        .post(url)
        .bearer_auth(api_key)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("{provider} request failed: {error}"))?;
    let status = response.status();
    let raw = response
        .text()
        .await
        .map_err(|error| format!("{provider} response read failed: {error}"))?;
    if !status.is_success() {
        return Err(format!("{provider} failed (status {status}): {raw}"));
    }
    serde_json::from_str(&raw)
        .map_err(|error| format!("Failed to parse {provider} response: {error}"))
}

// ---- search ----

pub struct TavilySearchProvider {
    api_key: String,
}

impl TavilySearchProvider {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl super::WebSearchProvider for TavilySearchProvider {
    fn name(&self) -> &'static str {
        "tavily"
    }

    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        let parsed: SearchResponseBody = post_json(
            SEARCH_URL,
            &self.api_key,
            &SearchRequestBody { query, max_results },
            "Tavily search",
        )
        .await?;
        Ok(project_search(parsed))
    }
}

#[derive(Serialize)]
struct SearchRequestBody<'a> {
    query: &'a str,
    max_results: usize,
}

#[derive(Deserialize)]
struct SearchResponseBody {
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    title: String,
    url: String,
    content: String,
}

fn project_search(response: SearchResponseBody) -> Vec<WebSearchResult> {
    response
        .results
        .into_iter()
        .map(|hit| WebSearchResult {
            title: hit.title,
            url: hit.url,
            snippet: hit.content,
        })
        .collect()
}

// ---- extract（fetch）----

pub struct TavilyFetchProvider {
    api_key: String,
}

impl TavilyFetchProvider {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl super::WebFetchProvider for TavilyFetchProvider {
    fn name(&self) -> &'static str {
        "tavily"
    }

    async fn fetch(&self, url: &str) -> Result<WebFetchContent, String> {
        let parsed: ExtractResponseBody = post_json(
            EXTRACT_URL,
            &self.api_key,
            &ExtractRequestBody {
                urls: [url.to_string()],
                format: "markdown",
            },
            "Tavily extract",
        )
        .await?;
        project_extract(parsed, url)
    }
}

#[derive(Serialize)]
struct ExtractRequestBody {
    urls: [String; 1],
    format: &'static str,
}

#[derive(Deserialize)]
struct ExtractResponseBody {
    #[serde(default)]
    results: Vec<ExtractedPage>,
    #[serde(default)]
    failed_results: Vec<FailedExtract>,
}

#[derive(Deserialize)]
struct ExtractedPage {
    url: String,
    #[serde(default)]
    raw_content: String,
}

#[derive(Deserialize)]
struct FailedExtract {
    url: String,
    #[serde(default)]
    error: String,
}

/// HTTP 200 也可能全部失败：`results` 与 `failed_results` 需同时检查（按 url 匹配）
fn project_extract(response: ExtractResponseBody, url: &str) -> Result<WebFetchContent, String> {
    if let Some(page) = response.results.iter().find(|page| page.url == url)
        && !page.raw_content.is_empty()
    {
        return Ok(WebFetchContent {
            title: String::new(),
            content: page.raw_content.clone(),
        });
    }
    let reason = response
        .failed_results
        .iter()
        .find(|failed| failed.url == url)
        .map(|failed| failed.error.as_str())
        .unwrap_or("no content returned");
    Err(format!("Tavily extract failed for {url}: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixture 依据 Tavily 官方文档响应结构构造
    #[test]
    fn projects_search_results_in_order() {
        const FIXTURE: &str = r#"{
            "query": "rust async",
            "answer": "",
            "images": [],
            "response_time": 0.68,
            "results": [
                { "title": "Tokio Tutorial", "url": "https://tokio.rs/tokio/tutorial", "content": "shared state", "score": 0.91 },
                { "title": "Async Book", "url": "https://rust-lang.github.io/async-book/", "content": "futures", "score": 0.82 }
            ]
        }"#;
        let parsed: SearchResponseBody = serde_json::from_str(FIXTURE).unwrap();
        let results = project_search(parsed);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Tokio Tutorial");
        assert_eq!(results[1].snippet, "futures");
    }

    #[test]
    fn extract_takes_matching_page_content() {
        const FIXTURE: &str = r##"{
            "results": [
                { "url": "https://example.com/post", "raw_content": "# Main\n\nbody text" }
            ],
            "failed_results": [],
            "response_time": 1.2
        }"##;
        let parsed: ExtractResponseBody = serde_json::from_str(FIXTURE).unwrap();
        let content = project_extract(parsed, "https://example.com/post").unwrap();

        assert_eq!(content.content, "# Main\n\nbody text");
    }

    #[test]
    fn extract_reports_failed_results() {
        const FIXTURE: &str = r#"{
            "results": [],
            "failed_results": [
                { "url": "https://example.com/blocked", "error": "403 Forbidden" }
            ],
            "response_time": 0.4
        }"#;
        let parsed: ExtractResponseBody = serde_json::from_str(FIXTURE).unwrap();
        let error = project_extract(parsed, "https://example.com/blocked").unwrap_err();

        assert!(error.contains("403 Forbidden"), "unexpected: {error}");
    }
}
