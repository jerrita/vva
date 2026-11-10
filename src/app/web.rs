use crate::config::{AppConfig, model::WebSettings};
use crate::tools::web::{brave, deepseek, google, local, tavily, zhipu, *};
use std::sync::Arc;
/// 会话 web 开关开启时调用。配置错误（能力不匹配、provider 未声明、密钥缺失）
/// 直接报错，与会话寻址错误同级；`settings.web` 未配置时为空集合
pub fn resolve(config: &AppConfig) -> Result<ExternalWeb, String> {
    let Some(web) = config.settings.web.as_ref() else {
        return Ok(ExternalWeb {
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
    Ok(ExternalWeb { search, fetch })
}

fn resolve_search(
    config: &AppConfig,
    web: &WebSettings,
    name: &str,
) -> Result<Arc<dyn WebSearchProvider>, String> {
    match name {
        "zhipu" => {
            let api_key = required_api_key(web, "zhipu")?;
            let search_engine = web
                .provider
                .as_ref()
                .and_then(|provider| provider.zhipu.as_ref())
                .ok_or("settings.web.provider.zhipu is required")?
                .search_engine;
            Ok(Arc::new(zhipu::ZhipuSearchProvider::new(
                api_key,
                search_engine,
                zhipu::ClientInfo {
                    name: "vva".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            )))
        }
        "deepseek" => Ok(Arc::new(deepseek::DeepSeekSearchProvider::new(
            required_api_key(web, "deepseek")?,
            deepseek_api_base(web),
        ))),
        "google" => {
            let resolved = resolve_google(config, google_model(web)?)?;
            Ok(Arc::new(google::GoogleSearchProvider::new(
                resolved.create_provider(),
                resolved.model_id.clone(),
                resolved.model_config.service_tier().map(str::to_string),
            )))
        }
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
        "deepseek" => Ok(Arc::new(deepseek::DeepSeekFetchProvider::new(
            required_api_key(web, "deepseek")?,
            deepseek_api_base(web),
        ))),
        "google" => {
            let resolved = resolve_google(config, google_model(web)?)?;
            Ok(Arc::new(google::GoogleFetchProvider::new(
                resolved.create_provider(),
                resolved.model_id.clone(),
                resolved.model_config.service_tier().map(str::to_string),
            )))
        }
        "local" => Ok(Arc::new(local::LocalFetchProvider)),
        "tavily" => Ok(Arc::new(tavily::TavilyFetchProvider::new(
            required_api_key(web, "tavily")?,
        ))),
        "brave" | "zhipu" => Err(format!(
            "settings.web.fetch = {name:?}: provider '{name}' only supports web search"
        )),
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

fn deepseek_api_base(web: &WebSettings) -> Option<&str> {
    web.provider
        .as_ref()
        .and_then(|provider| provider.deepseek.as_ref())
        .and_then(|deepseek| deepseek.api_base.as_deref())
}

fn required_api_key(web: &WebSettings, name: &str) -> Result<String, String> {
    let auth = match name {
        "zhipu" => web
            .provider
            .as_ref()
            .and_then(|provider| provider.zhipu.as_ref())
            .map(|auth| auth.api_key.as_str()),
        "deepseek" => web
            .provider
            .as_ref()
            .and_then(|provider| provider.deepseek.as_ref())
            .map(|auth| auth.api_key.as_str()),
        "brave" => web
            .provider
            .as_ref()
            .and_then(|provider| provider.brave.as_ref())
            .map(|auth| auth.api_key.as_str()),
        "tavily" => web
            .provider
            .as_ref()
            .and_then(|provider| provider.tavily.as_ref())
            .map(|auth| auth.api_key.as_str()),
        _ => None,
    };
    let api_key = auth.map(|key| key.trim().to_string()).unwrap_or_default();
    if api_key.is_empty() {
        return Err(format!(
            "settings.web.provider.{name}.api_key is required when selecting provider '{name}'"
        ));
    }
    Ok(api_key)
}

fn resolve_google<'a>(
    config: &'a AppConfig,
    spec: &str,
) -> Result<crate::config::ResolvedModel<'a>, String> {
    let resolved = config
        .resolve_model(spec, None)
        .map_err(|error| format!("settings.web.provider.google.model: {error}"))?;
    if resolved.effective_type() != crate::provider::ProviderType::Gemini {
        return Err(format!(
            "settings.web.provider.google.model = {spec:?}: model must use the gemini protocol"
        ));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(resolve(&AppConfig::default()).unwrap().search.is_none());
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
        let external = resolve(&config_with(web)).unwrap();
        assert_eq!(external.search.unwrap().name(), "brave");
        assert_eq!(external.fetch.unwrap().name(), "tavily");
    }

    #[test]
    fn resolve_local_fetch_without_declaration() {
        let web = settings(r#"{"fetch":"local"}"#);
        let external = resolve(&config_with(web)).unwrap();
        assert_eq!(external.fetch.unwrap().name(), "local");
    }

    #[test]
    fn resolve_deepseek_search_and_fetch() {
        let web = settings(
            r#"{
                "provider": {"deepseek": {"api_key": "test-key"}},
                "search": "deepseek",
                "fetch": "deepseek"
            }"#,
        );
        assert_eq!(web.declared_names(), ["deepseek"]);
        let external = resolve(&config_with(web)).unwrap();
        assert_eq!(external.search.unwrap().name(), "deepseek");
        assert_eq!(external.fetch.unwrap().name(), "deepseek");

        for kind in ["search", "fetch"] {
            for declaration in ["{}", r#"{"deepseek":{"api_key":"   "}}"#] {
                let web = settings(&format!(
                    r#"{{"{kind}":"deepseek","provider":{declaration}}}"#
                ));
                let error = resolve(&config_with(web)).err().unwrap();
                assert!(error.contains("settings.web.provider.deepseek.api_key"));
            }
        }
    }

    #[test]
    fn resolve_zhipu_engines_and_validate_configuration() {
        for engine in [
            None,
            Some("search_std"),
            Some("search_pro"),
            Some("search_pro_sogou"),
            Some("search_pro_quark"),
            Some("search_mcp"),
        ] {
            let mut value = serde_json::json!({
                "provider":{"zhipu":{"api_key":"test-key"}}, "search":"zhipu"
            });
            if let Some(engine) = engine {
                value["provider"]["zhipu"]["search_engine"] = engine.into();
            }
            let web: WebSettings = serde_json::from_value(value).unwrap();
            assert_eq!(web.declared_names(), ["zhipu"]);
            let external = resolve(&config_with(web)).unwrap();
            assert_eq!(external.search.unwrap().name(), "zhipu");
            assert!(external.fetch.is_none());
        }
        for declaration in ["{}", r#"{"zhipu":{"api_key":"   "}}"#] {
            let web = settings(&format!(r#"{{"search":"zhipu","provider":{declaration}}}"#));
            let error = resolve(&config_with(web)).err().unwrap();
            assert!(error.contains("settings.web.provider.zhipu.api_key"));
        }
        let web = settings(r#"{"fetch":"zhipu"}"#);
        assert!(
            resolve(&config_with(web))
                .err()
                .unwrap()
                .contains("only supports web search")
        );
        assert!(
            serde_json::from_str::<WebSettings>(
                r#"{
            "provider":{"zhipu":{"api_key":"test-key","search_engine":"unknown"}}
        }"#
            )
            .is_err()
        );
    }

    #[test]
    fn resolve_rejects_capability_mismatch_and_unknown_names() {
        let mismatch = settings(r#"{"search":"local"}"#);
        assert!(resolve(&config_with(mismatch)).is_err());
        let mismatch = settings(r#"{"fetch":"brave"}"#);
        assert!(resolve(&config_with(mismatch)).is_err());
        let unknown = settings(r#"{"search":"serp"}"#);
        assert!(resolve(&config_with(unknown)).is_err());
    }

    #[test]
    fn resolve_google_requires_gemini_model() {
        let build = |kind: &str, model: &str| {
            config_from(&format!(
                r#"{{
                    "providers": {{
                        "g": {{"type":"gemini","api_key":"k","models":{{"m":{{"name":"m"}}}}}},
                        "r": {{"type":"response","api_key":"k","models":{{"m":{{"name":"m"}}}}}}
                    }},
                    "settings": {{"default":"g/m","web":{{"{kind}":"google","provider":{{"google":{{"model":"{model}"}}}}}}}}
                }}"#
            ))
        };
        let external = resolve(&build("search", "g/m")).unwrap();
        assert_eq!(external.search.unwrap().name(), "google");
        let external = resolve(&build("fetch", "g/m")).unwrap();
        assert_eq!(external.fetch.unwrap().name(), "google");
        for model in ["r/m", "", "g/missing"] {
            assert!(resolve(&build("search", model)).is_err(), "{model}");
        }
    }
}
