//! 智谱 Web Search API 与 Coding Plan Search MCP（仅 search）。
//! https://docs.bigmodel.cn/api-reference/工具-api/网络搜索
//! https://docs.bigmodel.cn/cn/coding-plan/mcp/search-mcp-server

mod mcp;
#[cfg(test)]
mod tests;

use std::{sync::OnceLock, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::WebSearchResult;
use crate::config::model::ZhipuSearchEngine;

const SEARCH_URL: &str = "https://open.bigmodel.cn/api/paas/v4/web_search";
const MCP_URL: &str = "https://open.bigmodel.cn/api/mcp/web_search_prime/mcp";
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .timeout(SEARCH_TIMEOUT)
            .build()
            .expect("failed to build Zhipu HTTP client")
    })
}

pub struct ZhipuSearchProvider {
    api_key: String,
    search_engine: ZhipuSearchEngine,
}

impl ZhipuSearchProvider {
    pub fn new(api_key: String, search_engine: ZhipuSearchEngine) -> Self {
        Self {
            api_key,
            search_engine,
        }
    }

    async fn rest_search(
        &self,
        endpoint: &str,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        let count = match self.search_engine {
            // 官方 sogou 仅接受 10、20、30、40、50；quark 不支持 count。
            ZhipuSearchEngine::SearchProSogou => Some(max_results.div_ceil(10) * 10),
            ZhipuSearchEngine::SearchProQuark => None,
            _ => Some(max_results),
        };
        let response = client()
            .post(endpoint)
            .bearer_auth(&self.api_key)
            .json(&SearchRequest {
                search_query: query,
                search_engine: self.search_engine,
                search_intent: false,
                count,
            })
            .send()
            .await
            .map_err(|_| "Zhipu search request failed".to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("Zhipu search failed (status {status})"));
        }
        let response: SearchResponse = response
            .json()
            .await
            .map_err(|_| "Invalid Zhipu search response".to_string())?;
        if let Some(error) = response.error {
            return Err(format!("Zhipu search API error (code {})", error.code));
        }
        let hits = response
            .search_result
            .ok_or("Zhipu search response is missing search_result")?;
        Ok(project(hits, max_results))
    }
}

#[async_trait]
impl super::WebSearchProvider for ZhipuSearchProvider {
    fn name(&self) -> &'static str {
        "zhipu"
    }

    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        let max_results = max_results.clamp(1, 50);
        tokio::time::timeout(SEARCH_TIMEOUT, async {
            match self.search_engine {
                ZhipuSearchEngine::SearchMcp => {
                    mcp::search(MCP_URL, &self.api_key, query, max_results).await
                }
                _ => self.rest_search(SEARCH_URL, query, max_results).await,
            }
        })
        .await
        .map_err(|_| "Zhipu search timed out after 60 seconds".to_string())?
    }
}

#[derive(Serialize)]
struct SearchRequest<'a> {
    search_query: &'a str,
    search_engine: ZhipuSearchEngine,
    search_intent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
}

#[derive(Deserialize)]
struct SearchResponse {
    search_result: Option<Vec<SearchHit>>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
}

#[derive(Deserialize)]
struct SearchHit {
    title: String,
    link: String,
    content: String,
}

fn project(hits: Vec<SearchHit>, max_results: usize) -> Vec<WebSearchResult> {
    hits.into_iter()
        .take(max_results)
        .map(|hit| WebSearchResult {
            title: hit.title,
            url: hit.link,
            snippet: hit.content,
        })
        .collect()
}
