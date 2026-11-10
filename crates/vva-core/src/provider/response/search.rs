use crate::model::{
    ResponseWebSearchAction, ResponseWebSearchSource, SearchStatus, WebSearchAction, WebSearchCall,
    WebSearchSource,
};
use crate::provider::ProviderError;
/// 流式条目和最终响应共用同一领域投影。
pub(super) fn project_web_search_call(
    id: &str,
    status: &str,
    action: Option<&ResponseWebSearchAction>,
) -> Result<WebSearchCall, ProviderError> {
    let status = match status {
        "in_progress" | "searching" => SearchStatus::Searching,
        "completed" => SearchStatus::Completed,
        "failed" | "incomplete" => SearchStatus::Failed,
        _ => {
            return Err(ProviderError::Protocol(format!(
                "Invalid web search status: {status}"
            )));
        }
    };
    let (action, query, sources) = match action {
        Some(ResponseWebSearchAction::Search {
            query,
            queries,
            sources,
        }) => (
            Some(WebSearchAction::Search),
            if queries.is_empty() {
                query.clone().unwrap_or_default()
            } else {
                queries.join(", ")
            },
            sources
                .iter()
                .filter_map(|src| match src {
                    ResponseWebSearchSource::Url { url } => {
                        Some(WebSearchSource::new(url, url, None))
                    }
                    // API 来源没有可引用的 URL
                    ResponseWebSearchSource::Api { .. } => None,
                })
                .collect(),
        ),
        Some(ResponseWebSearchAction::OpenPage { url }) => (
            Some(WebSearchAction::OpenPage {
                url: url.clone().unwrap_or_default(),
            }),
            String::new(),
            Vec::new(),
        ),
        Some(ResponseWebSearchAction::FindInPage { url, pattern }) => (
            Some(WebSearchAction::FindInPage {
                url: url.clone(),
                pattern: pattern.clone(),
            }),
            String::new(),
            Vec::new(),
        ),
        // 端点未下发动作：如实保留缺席（领域层 None），不伪造空 Search
        None => (None, String::new(), Vec::new()),
    };
    Ok(WebSearchCall {
        id: id.to_string(),
        action,
        query,
        status,
        sources,
    })
}
