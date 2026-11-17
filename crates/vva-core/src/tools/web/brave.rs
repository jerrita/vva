//! Brave Search LLM Context provider（仅 search）。
//! `GET https://api.search.brave.com/res/v1/llm/context`，认证 header `X-Subscription-Token`；
//! 响应 `grounding.generic[]` 为按 url 组织的内容块，`sources` 为 url → 页面元数据。

use std::collections::HashMap;

use async_trait::async_trait;
use serde::Deserialize;

use super::WebSearchResult;
use crate::provider::shared_client;

const LLM_CONTEXT_URL: &str = "https://api.search.brave.com/res/v1/llm/context";
const SUBSCRIPTION_TOKEN_HEADER: &str = "X-Subscription-Token";

pub struct BraveSearchProvider {
    api_key: String,
}

impl BraveSearchProvider {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl super::WebSearchProvider for BraveSearchProvider {
    fn name(&self) -> &'static str {
        "brave"
    }

    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        let count = max_results.to_string();
        let response = shared_client()
            .get(LLM_CONTEXT_URL)
            .query(&[("q", query), ("count", count.as_str())])
            .header(SUBSCRIPTION_TOKEN_HEADER, &self.api_key)
            .send()
            .await
            .map_err(|error| format!("Brave search request failed: {error}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| format!("Brave search response read failed: {error}"))?;
        if !status.is_success() {
            return Err(format!("Brave search failed (status {status}): {body}"));
        }
        let parsed: LlmContextResponse = serde_json::from_str(&body)
            .map_err(|error| format!("Failed to parse Brave search response: {error}"))?;
        Ok(project_llm_context(parsed))
    }
}

#[derive(Deserialize)]
struct LlmContextResponse {
    grounding: Grounding,
    #[serde(default)]
    sources: HashMap<String, SourceMeta>,
}

#[derive(Deserialize)]
struct Grounding {
    #[serde(default)]
    generic: Vec<GenericChunk>,
}

#[derive(Deserialize)]
struct GenericChunk {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    snippets: Vec<String>,
}

#[derive(Deserialize)]
struct SourceMeta {
    #[serde(default)]
    title: Option<String>,
}

/// `grounding.generic[]` 按 url 聚合：合并同 url 的 snippets，
/// title 优先取条目自身，缺省回落 `sources[url].title`
fn project_llm_context(response: LlmContextResponse) -> Vec<WebSearchResult> {
    let mut order: Vec<String> = Vec::new();
    let mut merged: HashMap<String, (Option<String>, Vec<String>)> = HashMap::new();
    for chunk in response.grounding.generic {
        let entry = merged.entry(chunk.url.clone()).or_insert_with(|| {
            order.push(chunk.url.clone());
            (chunk.title.clone(), Vec::new())
        });
        if entry.0.is_none() {
            entry.0 = chunk.title;
        }
        entry.1.extend(chunk.snippets);
    }
    order
        .into_iter()
        .filter_map(|url| {
            let (title, snippets) = merged.remove(&url)?;
            let title = title
                .or_else(|| {
                    response
                        .sources
                        .get(&url)
                        .and_then(|meta| meta.title.clone())
                })
                .unwrap_or_default();
            Some(WebSearchResult {
                title,
                url,
                snippet: snippets.join("\n"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixture 依据 Brave LLM Context 官方文档响应结构构造
    const FIXTURE: &str = r#"{
        "grounding": {
            "generic": [
                { "url": "https://example.com/a", "title": "Example A", "snippets": ["alpha text", "beta text"] },
                { "url": "https://example.com/b", "title": "Example B", "snippets": ["gamma text"] },
                { "url": "https://example.com/a", "snippets": ["delta text"] }
            ]
        },
        "sources": {
            "https://example.com/a": { "title": "Source Title A", "hostname": "example.com", "age": "2026-09-01T00:00:00Z" },
            "https://example.com/b": { "title": "Source Title B", "hostname": "example.com", "age": "2026-09-02T00:00:00Z" }
        }
    }"#;

    #[test]
    fn projects_grounding_chunks_grouped_by_url() {
        let parsed: LlmContextResponse = serde_json::from_str(FIXTURE).unwrap();
        let results = project_llm_context(parsed);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://example.com/a");
        assert_eq!(results[0].title, "Example A");
        assert_eq!(results[0].snippet, "alpha text\nbeta text\ndelta text");
        assert_eq!(results[1].url, "https://example.com/b");
        assert_eq!(results[1].snippet, "gamma text");
    }

    #[test]
    fn falls_back_to_source_title_when_chunk_title_missing() {
        const FIXTURE: &str = r#"{
            "grounding": {
                "generic": [ { "url": "https://example.com/c", "snippets": ["text"] } ]
            },
            "sources": {
                "https://example.com/c": { "title": "Only In Sources" }
            }
        }"#;
        let parsed: LlmContextResponse = serde_json::from_str(FIXTURE).unwrap();
        let results = project_llm_context(parsed);

        assert_eq!(results[0].title, "Only In Sources");
    }
}
