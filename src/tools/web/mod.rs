//! 外置 web search / fetch provider：为原生 server tool 能力缺位的模型提供客户端工具补全。
//! 能力矩阵：`brave` 仅 search；`google` 二者皆支持（单模型原生 google_search / url_context）；`local` 仅 fetch（内置，无需声明）；`tavily` 二者皆支持。

pub mod brave;
pub mod google;
pub mod local;
pub mod tavily;
pub mod tool;

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::AppConfig;
use crate::config::model::WebSettings;

/// 一条 web search 结果
#[derive(Debug)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 一次 web fetch 的页面正文
#[derive(Debug)]
pub struct WebFetchContent {
    pub title: String,
    pub content: String,
}

#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    /// provider 名（Notice 与激活提示用）
    fn name(&self) -> &'static str;

    async fn search(&self, query: &str, max_results: usize)
    -> Result<Vec<WebSearchResult>, String>;
}

#[async_trait]
pub trait WebFetchProvider: Send + Sync {
    fn name(&self) -> &'static str;

    async fn fetch(&self, url: &str) -> Result<WebFetchContent, String>;
}

/// 按 `settings.web` 解析出的外置 provider 集合；未选择的能力位为 `None`
pub struct ExternalWeb {
    pub search: Option<Arc<dyn WebSearchProvider>>,
    pub fetch: Option<Arc<dyn WebFetchProvider>>,
}

impl ExternalWeb {
    /// 会话 web 开关开启时调用。配置错误（能力不匹配、provider 未声明、密钥缺失）
    /// 直接报错，与会话寻址错误同级；`settings.web` 未配置时为空集合
    pub fn resolve(config: &AppConfig) -> Result<Self, String> {
        let Some(web) = config.settings.web.as_ref() else {
            return Ok(Self {
                search: None,
                fetch: None,
            });
        };
        let search = match web.search.as_deref() {
            None => None,
            Some(name) => Some(resolve_search(config, web, name)?),
        };
        let fetch = match web.fetch.as_deref() {
            None => None,
            Some(name) => Some(resolve_fetch(config, web, name)?),
        };
        Ok(Self { search, fetch })
    }
}

fn resolve_search(
    config: &AppConfig,
    web: &WebSettings,
    name: &str,
) -> Result<Arc<dyn WebSearchProvider>, String> {
    match name {
        "google" => Ok(Arc::new(google::GoogleSearchProvider::new(
            config,
            google_model(web)?,
        )?)),
        "brave" => Ok(Arc::new(brave::BraveSearchProvider::new(required_api_key(
            web, "brave",
        )?))),
        "tavily" => Ok(Arc::new(tavily::TavilySearchProvider::new(
            required_api_key(web, "tavily")?,
        ))),
        "local" => Err(
            "settings.web.search = \"local\": provider 'local' only supports web fetch".to_string(),
        ),
        other => Err(format!(
            "settings.web.search = {other:?}: unknown web provider (declared: {:?}; builtin: \"local\", fetch-only)",
            web.declared_names()
        )),
    }
}

fn resolve_fetch(
    config: &AppConfig,
    web: &WebSettings,
    name: &str,
) -> Result<Arc<dyn WebFetchProvider>, String> {
    match name {
        "google" => Ok(Arc::new(google::GoogleFetchProvider::new(
            config,
            google_model(web)?,
        )?)),
        "local" => Ok(Arc::new(local::LocalFetchProvider)),
        "tavily" => Ok(Arc::new(tavily::TavilyFetchProvider::new(
            required_api_key(web, "tavily")?,
        ))),
        "brave" => Err(
            "settings.web.fetch = \"brave\": provider 'brave' only supports web search".to_string(),
        ),
        other => Err(format!(
            "settings.web.fetch = {other:?}: unknown web provider (declared: {:?}; builtin: \"local\")",
            web.declared_names()
        )),
    }
}

fn google_model(web: &WebSettings) -> Result<&str, String> {
    let spec = web
        .provider
        .as_ref()
        .and_then(|provider| provider.google.as_ref())
        .map(|google| google.model.trim())
        .unwrap_or_default();
    if spec.is_empty() {
        return Err(
            "settings.web.provider.google.model is required when selecting provider 'google'"
                .to_string(),
        );
    }
    Ok(spec)
}

fn required_api_key(web: &WebSettings, name: &str) -> Result<String, String> {
    let auth = match name {
        "brave" => web
            .provider
            .as_ref()
            .and_then(|provider| provider.brave.as_ref()),
        "tavily" => web
            .provider
            .as_ref()
            .and_then(|provider| provider.tavily.as_ref()),
        _ => None,
    };
    let api_key = auth
        .map(|auth| auth.api_key.trim().to_string())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Err(format!(
            "settings.web.provider.{name}.api_key is required when selecting provider '{name}'"
        ));
    }
    Ok(api_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{WebApiAuth, WebProviders};

    fn settings(json: &str) -> WebSettings {
        serde_json::from_str(json).unwrap()
    }

    fn config_with(web: WebSettings) -> AppConfig {
        let mut config = AppConfig::default();
        config.settings.web = Some(web);
        config
    }

    fn config_from(json: &str) -> AppConfig {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn resolve_empty_when_web_settings_absent_or_bare() {
        assert!(
            ExternalWeb::resolve(&AppConfig::default())
                .unwrap()
                .search
                .is_none()
        );
        let bare = settings(r#"{"search":"brave"}"#);
        assert!(required_api_key(&bare, "brave").is_err());
    }

    #[test]
    fn resolve_selects_declared_providers() {
        let web = settings(
            r#"{
                "provider": {
                    "brave": { "api_key": "k-brave" },
                    "tavily": { "api_key": "k-tavily" }
                },
                "search": "brave",
                "fetch": "tavily"
            }"#,
        );
        let external = ExternalWeb::resolve(&config_with(web)).unwrap();
        assert_eq!(external.search.unwrap().name(), "brave");
        assert_eq!(external.fetch.unwrap().name(), "tavily");
    }

    #[test]
    fn resolve_local_fetch_without_declaration() {
        let web = settings(r#"{"fetch":"local"}"#);
        let external = ExternalWeb::resolve(&config_with(web)).unwrap();
        assert_eq!(external.fetch.unwrap().name(), "local");
    }

    #[test]
    fn resolve_rejects_capability_mismatch_and_unknown_names() {
        let mismatch = settings(r#"{"search":"local"}"#);
        assert!(ExternalWeb::resolve(&config_with(mismatch)).is_err());
        let mismatch = settings(r#"{"fetch":"brave"}"#);
        assert!(ExternalWeb::resolve(&config_with(mismatch)).is_err());
        let unknown = settings(r#"{"search":"serp"}"#);
        assert!(ExternalWeb::resolve(&config_with(unknown)).is_err());
    }

    #[test]
    fn resolve_google_requires_gemini_model() {
        let web = r#""settings":{"default":"g/m","web":{"search":"google","provider":{"google":{"model":"MODEL"}}}}"#;
        let providers = r#""providers":{
            "g":{"type":"gemini","api_key":"k","models":{"m":{"name":"m"}}},
            "r":{"type":"response","api_key":"k","models":{"m":{"name":"m"}}}
        }"#;
        let build =
            |model: &str| config_from(&format!("{{{providers},{}}}", web.replace("MODEL", model)));
        let external = ExternalWeb::resolve(&build("g/m")).unwrap();
        assert_eq!(external.search.unwrap().name(), "google");
        assert!(ExternalWeb::resolve(&build("r/m")).is_err());
        let fetch = ExternalWeb::resolve(&config_from(&format!(
            "{{{providers},{}}}",
            web.replace("MODEL", "g/m")
                .replace("\"search\"", "\"fetch\"")
        )))
        .unwrap();
        assert_eq!(fetch.fetch.unwrap().name(), "google");
        assert!(ExternalWeb::resolve(&build("")).is_err());
        assert!(ExternalWeb::resolve(&build("g/missing")).is_err());
    }

    #[test]
    fn required_api_key_rejects_missing_or_blank() {
        let undeclared = settings(r#"{"search":"tavily"}"#);
        assert!(required_api_key(&undeclared, "tavily").is_err());
        let blank = WebSettings {
            provider: Some(WebProviders {
                brave: None,
                google: None,
                tavily: Some(WebApiAuth {
                    api_key: "   ".to_string(),
                }),
            }),
            search: None,
            fetch: None,
        };
        assert!(required_api_key(&blank, "tavily").is_err());
    }
}
