use crate::model::content::{Annotation, WebSearchCall, WebSearchSource};
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("Search request failed: {0}")]
    RequestFailed(String),
    #[error("Search provider error: {0}")]
    ProviderError(String),
}

/// 客户端外挂搜索引擎 Trait（当模型供应商不具备原生 Grounding 时的备用机制，如 Tavily / SearxNG）
#[async_trait]
pub trait ExternalSearchEngine: Send + Sync {
    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<WebSearchSource>, SearchError>;
}

/// 统一搜索引擎调度器：整合原生 Grounding 与外置搜索引擎
pub struct UnifiedSearchService {
    fallback_engine: Option<Box<dyn ExternalSearchEngine>>,
}

impl UnifiedSearchService {
    pub fn new(fallback_engine: Option<Box<dyn ExternalSearchEngine>>) -> Self {
        Self { fallback_engine }
    }

    /// 执行外置检索并组装为 Responses 风格的 WebSearchCall 和 Annotation
    pub async fn execute_fallback_search(
        &self,
        call_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<(WebSearchCall, Vec<Annotation>), SearchError> {
        let engine = self.fallback_engine.as_ref().ok_or_else(|| {
            SearchError::ProviderError("No fallback search engine configured".into())
        })?;

        let sources = engine.search(query, max_results).await?;
        let call = WebSearchCall::completed(call_id, query, sources.clone());

        let mut annotations = Vec::new();
        for source in &sources {
            annotations.push(Annotation::UrlCitation {
                url: source.url.clone(),
                title: source.title.clone(),
                start_index: 0,
                end_index: 0,
            });
        }

        Ok((call, annotations))
    }
}
