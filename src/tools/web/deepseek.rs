//! DeepSeek Search / Fetch 原生 JSON API，使用 Bearer 认证。
//! 协议：https://api-docs.deepseek.com/guides/search/

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{WebFetchContent, WebSearchResult};
use crate::provider::shared_client;

const DEFAULT_API_BASE: &str = "https://api.deepseek.com";

async fn post_json<T: serde::de::DeserializeOwned>(
    url: &str,
    api_key: &str,
    body: &impl Serialize,
    operation: &str,
) -> Result<T, String> {
    let response = shared_client()
        .post(url)
        .bearer_auth(api_key)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("DeepSeek {operation} request failed: {error}"))?;
    let status = response.status();
    let raw = response
        .text()
        .await
        .map_err(|error| format!("DeepSeek {operation} response read failed: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "DeepSeek {operation} failed (status {status}): {raw}"
        ));
    }
    serde_json::from_str(&raw)
        .map_err(|error| format!("Failed to parse DeepSeek {operation} response: {error}"))
}

pub struct DeepSeekSearchProvider {
    api_key: String,
    endpoint: String,
}

impl DeepSeekSearchProvider {
    pub fn new(api_key: String, api_base: Option<&str>) -> Self {
        Self {
            api_key,
            endpoint: format!(
                "{}/v1/tools/search",
                api_base.unwrap_or(DEFAULT_API_BASE).trim_end_matches('/')
            ),
        }
    }
}

#[async_trait]
impl super::WebSearchProvider for DeepSeekSearchProvider {
    fn name(&self) -> &'static str {
        "deepseek"
    }

    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        let response: SearchResponse = post_json(
            &self.endpoint,
            &self.api_key,
            &SearchRequest {
                query,
                count: max_results,
                need_content: false,
            },
            "search",
        )
        .await?;
        Ok(project_search(response))
    }
}

#[derive(Serialize)]
struct SearchRequest<'a> {
    query: &'a str,
    count: usize,
    need_content: bool,
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    title: String,
    url: String,
    summary: String,
}

fn project_search(response: SearchResponse) -> Vec<WebSearchResult> {
    response
        .results
        .into_iter()
        .map(|hit| WebSearchResult {
            title: hit.title,
            url: hit.url,
            // 官方 summary 面向模型，snippet 面向 UI。
            snippet: hit.summary,
        })
        .collect()
}

pub struct DeepSeekFetchProvider {
    api_key: String,
    endpoint: String,
}

impl DeepSeekFetchProvider {
    pub fn new(api_key: String, api_base: Option<&str>) -> Self {
        Self {
            api_key,
            endpoint: format!(
                "{}/v1/tools/fetch",
                api_base.unwrap_or(DEFAULT_API_BASE).trim_end_matches('/')
            ),
        }
    }
}

#[async_trait]
impl super::WebFetchProvider for DeepSeekFetchProvider {
    fn name(&self) -> &'static str {
        "deepseek"
    }

    async fn fetch(&self, url: &str) -> Result<WebFetchContent, String> {
        let response: FetchResponse = post_json(
            &self.endpoint,
            &self.api_key,
            &FetchRequest { url },
            "fetch",
        )
        .await?;
        Ok(WebFetchContent {
            title: response.title,
            content: response.content,
        })
    }
}

#[derive(Serialize)]
struct FetchRequest<'a> {
    url: &'a str,
}

#[derive(Deserialize)]
struct FetchResponse {
    title: String,
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_endpoints_from_default_or_custom_base() {
        for (base, expected) in [
            (None, "https://api.deepseek.com"),
            (Some("https://example.com"), "https://example.com"),
            (Some("https://example.com/"), "https://example.com"),
            (
                Some("https://example.com/proxy/"),
                "https://example.com/proxy",
            ),
        ] {
            assert_eq!(
                DeepSeekSearchProvider::new("test-key".into(), base).endpoint,
                format!("{expected}/v1/tools/search")
            );
            assert_eq!(
                DeepSeekFetchProvider::new("test-key".into(), base).endpoint,
                format!("{expected}/v1/tools/fetch")
            );
        }
    }

    #[test]
    fn serializes_native_requests() {
        assert_eq!(
            serde_json::to_value(SearchRequest {
                query: "async site:example.com",
                count: 5,
                need_content: false,
            })
            .unwrap(),
            json!({"query": "async site:example.com", "count": 5, "need_content": false})
        );
        assert_eq!(
            serde_json::to_value(FetchRequest {
                url: "https://example.com/article",
            })
            .unwrap(),
            json!({"url": "https://example.com/article"})
        );
    }

    #[test]
    fn projects_model_summary_and_preserves_order() {
        let response: SearchResponse = serde_json::from_value(json!({
            "results": [
                {
                    "rerank_score": 2.4,
                    "title": "Async guide",
                    "url": "https://example.com/async",
                    "snippet": "Short UI snippet",
                    "summary": "Detailed model summary",
                    "content": "",
                    "site_name": "Example",
                    "site_icon": null,
                    "date": null,
                    "authority": 3
                },
                {
                    "rerank_score": 1.8,
                    "title": "Runtime guide",
                    "url": "https://example.com/runtime",
                    "snippet": "Runtime",
                    "summary": "",
                    "content": "",
                    "site_name": null,
                    "site_icon": null,
                    "date": null,
                    "authority": -1
                }
            ]
        }))
        .unwrap();
        let results = project_search(response);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Async guide");
        assert_eq!(results[0].url, "https://example.com/async");
        assert_eq!(results[0].snippet, "Detailed model summary");
        assert_eq!(results[1].title, "Runtime guide");
        assert!(results[1].snippet.is_empty());
    }

    #[test]
    fn accepts_empty_search_and_rejects_missing_fields() {
        let response = serde_json::from_str(r#"{"results":[]}"#).unwrap();
        assert!(project_search(response).is_empty());
        for raw in [r#"{}"#, r#"{"results":[{"title":"t","url":"u"}]}"#] {
            assert!(serde_json::from_str::<SearchResponse>(raw).is_err());
        }
        assert!(serde_json::from_str::<FetchResponse>(r#"{"title":"t"}"#).is_err());
    }

    #[tokio::test]
    async fn sends_bearer_json_and_reports_api_errors() {
        use axum::{Json, Router, http::HeaderMap, routing::post};

        let app = Router::new()
            .route(
                "/proxy/v1/tools/fetch",
                post(
                    |headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                        assert_eq!(headers["authorization"], "Bearer test-key");
                        assert_eq!(headers["content-type"], "application/json");
                        assert_eq!(body, json!({"url":"https://example.com/article"}));
                        Json(json!({"title":"","content":"# Article\n\nBody"}))
                    },
                ),
            )
            .route(
                "/proxy/v1/tools/search",
                post(
                    |headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                        assert_eq!(headers["authorization"], "Bearer test-key");
                        assert_eq!(
                            body,
                            json!({"query":"async","count":3,"need_content":false})
                        );
                        Json(json!({"results":[]}))
                    },
                ),
            )
            .route(
                "/error",
                post(|| async {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"error": {
                            "message": "QPS limit exceeded",
                            "type": "rate_limit_error",
                            "param": null,
                            "code": "rate_limit_error"
                        }})),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = serde_json::from_value(json!({
            "settings": {
                "web": {
                    "provider": {
                        "deepseek": {"api_key":"test-key","api_base":format!("{base}/proxy/")}
                    },
                    "search":"deepseek",
                    "fetch":"deepseek"
                }
            }
        }))
        .unwrap();
        let external = super::super::ExternalWeb::resolve(&config).unwrap();
        let results = external.search.unwrap().search("async", 3).await.unwrap();
        assert!(results.is_empty());
        let response = external
            .fetch
            .unwrap()
            .fetch("https://example.com/article")
            .await
            .unwrap();
        let request = FetchRequest {
            url: "https://example.com/article",
        };
        assert!(response.title.is_empty());
        assert_eq!(response.content, "# Article\n\nBody");
        let error =
            post_json::<FetchResponse>(&format!("{base}/error"), "test-key", &request, "fetch")
                .await
                .err()
                .unwrap();
        server.abort();
        assert!(error.contains("429"), "{error}");
        assert!(error.contains("QPS limit exceeded"), "{error}");
    }
}
