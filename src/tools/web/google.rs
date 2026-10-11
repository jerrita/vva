//! Google provider：以已配置的 Gemini 模型执行原生内置工具，单次请求仅挂载对应工具（无函数声明）。
//! search：`google_search` grounding，结果取自 groundingChunks，片段取自被引用的文本段；
//! fetch：`url_context` 并同挂 `google_search`（页面无法直接读取时可由搜索补足），页面由服务端读取，正文为模型据此给出的 Markdown 转述。

use std::sync::Arc;

use async_trait::async_trait;

use super::{WebFetchContent, WebSearchResult};
use crate::config::AppConfig;
use crate::config::model::ProviderType;
use crate::model::{ContentPart, Message, Session, Turn, WebSearchSource};
use crate::provider::{CompletionOptions, Provider, ProviderResponse};

/// 已解析的 Gemini 模型句柄：search / fetch 共用
struct GoogleModel {
    provider: Arc<dyn Provider>,
    model_id: String,
    service_tier: Option<String>,
}

impl GoogleModel {
    /// `spec` 为 `provider/model`，须解析为 Gemini 协议模型
    fn new(config: &AppConfig, spec: &str) -> Result<Self, String> {
        let resolved = config
            .resolve_model(spec, None)
            .map_err(|error| format!("settings.web.provider.google.model: {error}"))?;
        if resolved.effective_type() != ProviderType::Gemini {
            return Err(format!(
                "settings.web.provider.google.model = {spec:?}: model must use the gemini protocol"
            ));
        }
        Ok(Self {
            provider: resolved.create_provider(),
            model_id: resolved.model_id.clone(),
            service_tier: resolved.model_config.service_tier().map(str::to_string),
        })
    }

    async fn ask(
        &self,
        prompt: String,
        options: CompletionOptions,
    ) -> Result<ProviderResponse, String> {
        let mut session = Session::new(".", None);
        session.append_turn(Turn::user_turn(Message::text(prompt)));
        let options = CompletionOptions {
            model: self.model_id.clone(),
            service_tier: self.service_tier.clone(),
            ..options
        };
        self.provider
            .complete(&session, &[], &options)
            .await
            .map_err(|error| format!("Google request failed: {error}"))
    }
}

pub struct GoogleSearchProvider(GoogleModel);

impl GoogleSearchProvider {
    pub fn new(config: &AppConfig, spec: &str) -> Result<Self, String> {
        GoogleModel::new(config, spec).map(Self)
    }
}

pub struct GoogleFetchProvider(GoogleModel);

impl GoogleFetchProvider {
    pub fn new(config: &AppConfig, spec: &str) -> Result<Self, String> {
        GoogleModel::new(config, spec).map(Self)
    }
}

#[async_trait]
impl super::WebFetchProvider for GoogleFetchProvider {
    fn name(&self) -> &'static str {
        "google"
    }

    async fn fetch(&self, url: &str) -> Result<WebFetchContent, String> {
        let response = self
            .0
            .ask(
                format!(
                    "Read {url} and reproduce its main textual content as Markdown. Keep the original wording, headings, lists, tables, and code blocks; omit navigation and ads. Do not summarize or add commentary."
                ),
                CompletionOptions {
                    enable_web_search: true,
                    enable_web_fetch: true,
                    ..Default::default()
                },
            )
            .await?;
        project_fetch(&response, url)
    }
}

fn project_fetch(response: &ProviderResponse, url: &str) -> Result<WebFetchContent, String> {
    let content: String = response
        .messages
        .iter()
        .map(Message::text_content)
        .collect();
    let content = content.trim();
    if content.is_empty() {
        return Err(format!("Google url_context returned no content for {url}"));
    }
    Ok(WebFetchContent {
        title: String::new(),
        content: content.to_string(),
    })
}

#[async_trait]
impl super::WebSearchProvider for GoogleSearchProvider {
    fn name(&self) -> &'static str {
        "google"
    }

    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        let response = self
            .0
            .ask(
                format!(
                    "Search the web for: {query}\nSummarize the most relevant findings concisely."
                ),
                CompletionOptions {
                    enable_web_search: true,
                    enable_web_fetch: false,
                    ..Default::default()
                },
            )
            .await?;
        Ok(project_search(&response, max_results))
    }
}

/// 来源按 grounding 顺序；每个来源的片段为引用它的文本段（按字节偏移切片）拼接
fn project_search(response: &ProviderResponse, max_results: usize) -> Vec<WebSearchResult> {
    let parts = || response.messages.iter().flat_map(|message| &message.parts);
    let mut sources: Vec<(WebSearchSource, Vec<&str>)> = Vec::new();
    for part in parts() {
        if let ContentPart::WebSearchCall(call) = part {
            for source in &call.sources {
                if !sources.iter().any(|(known, _)| known.url == source.url) {
                    sources.push((source.clone(), Vec::new()));
                }
            }
        }
    }
    for part in parts() {
        let ContentPart::Text { text, annotations } = part else {
            continue;
        };
        for annotation in annotations {
            if let crate::model::Annotation::UrlCitation {
                url,
                start_index,
                end_index,
                ..
            } = annotation
                && let Some((_, snippets)) = sources.iter_mut().find(|(known, _)| &known.url == url)
                && let Some(segment) = text.get(*start_index..*end_index)
                && !snippets.contains(&segment)
            {
                snippets.push(segment);
            }
        }
    }
    sources
        .into_iter()
        .take(max_results)
        .map(|(source, snippets)| WebSearchResult {
            title: source.title,
            url: source.url,
            snippet: snippets.join(" "),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Annotation, TurnMeta, WebSearchCall};

    #[test]
    fn projects_sources_with_cited_segments() {
        let text = "Tokio is a runtime. It powers async Rust.";
        let response = ProviderResponse {
            messages: vec![Message::new(vec![
                ContentPart::WebSearchCall(WebSearchCall::completed(
                    "s1",
                    "tokio",
                    vec![
                        WebSearchSource::new("https://tokio.rs", "Tokio", None),
                        WebSearchSource::new("https://example.com", "Other", None),
                    ],
                )),
                ContentPart::Text {
                    text: text.to_string(),
                    annotations: vec![Annotation::UrlCitation {
                        url: "https://tokio.rs".into(),
                        title: "Tokio".into(),
                        start_index: 0,
                        end_index: 19,
                    }],
                },
            ])],
            usage: None,
            meta: TurnMeta::default(),
        };
        let results = project_search(&response, 5);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet, "Tokio is a runtime.");
        assert_eq!(results[1].snippet, "");
        assert_eq!(project_search(&response, 1).len(), 1);
    }
}
