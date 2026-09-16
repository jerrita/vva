use crate::model::{
    ResponseWebSearchAction, SearchStatus, WebSearchAction, WebSearchCall, WebSearchSource,
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
        "failed" => SearchStatus::Failed,
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
            WebSearchAction::Search,
            query.clone().unwrap_or_else(|| queries.join(", ")),
            sources
                .iter()
                .map(|src| WebSearchSource::new(&src.url, &src.url, None))
                .collect(),
        ),
        Some(ResponseWebSearchAction::OpenPage { url }) => (
            WebSearchAction::OpenPage {
                url: url.clone().unwrap_or_default(),
            },
            String::new(),
            Vec::new(),
        ),
        Some(ResponseWebSearchAction::FindInPage { url, pattern }) => (
            WebSearchAction::FindInPage {
                url: url.clone(),
                pattern: pattern.clone(),
            },
            String::new(),
            Vec::new(),
        ),
        None => (WebSearchAction::Search, String::new(), Vec::new()),
    };
    Ok(WebSearchCall {
        id: id.to_string(),
        action,
        query,
        status,
        sources,
    })
}
