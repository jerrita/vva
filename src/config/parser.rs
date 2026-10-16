use std::fs;
use std::path::Path;
use thiserror::Error;

use super::model::AppConfig;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read config file {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("Failed to parse config JSON/JSONC: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Config file not found in search paths: {0:?}")]
    NotFound(Vec<String>),
    #[error("Model spec not found: {0}")]
    ModelNotFound(String),
    #[error("Invalid variant '{variant}' for model '{model}'. Valid variants: {valid:?}")]
    InvalidVariant {
        variant: String,
        model: String,
        valid: Vec<String>,
    },
}

/// 剥离 JSONC 中的单行注释 `// ...` 和块级注释 `/* ... */`，同时严格保留字符串内部字符
pub fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match (ch, chars.peek()) {
            ('"', _) => {
                out.push(ch);
                let mut escaped = false;
                for c in chars.by_ref() {
                    out.push(c);
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        break;
                    }
                }
            }
            // 单行注释：跳至行尾，保留换行符以维持报错行号
            ('/', Some('/')) => while chars.next_if(|c| *c != '\n').is_some() {},
            // 块级注释：跳至结束符 */，保留其中换行符
            ('/', Some('*')) => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                    }
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => out.push(ch),
        }
    }

    out
}

/// 展开文本中的 `${ENV_VAR}` 环境变量
pub fn interpolate_env_vars(input: &str) -> String {
    interpolate_with(input, |name| std::env::var(name).ok())
}

/// 按 `lookup` 展开文本中的 `${VAR}`，未命中展开为空字符串
pub(crate) fn interpolate_with(input: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start + 2..].find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&lookup(&rest[start + 2..start + 2 + len]).unwrap_or_default());
        rest = &rest[start + 3 + len..];
    }
    out.push_str(rest);
    out
}

/// 解析 JSON 或 JSONC 字符串（同时支持注释剔除与环境变量展开）
pub fn parse_config_str(input: &str) -> Result<AppConfig, ConfigError> {
    let stripped = strip_jsonc_comments(input);
    let interpolated = interpolate_env_vars(&stripped);
    let mut value: serde_json::Value = serde_json::from_str(&interpolated)?;
    // 在配置入口展开供应商预设，运行时只保留协议类型。
    // glm-coding-plan 不是合法协议名，需在反序列化前归一化为 response
    if let Some(providers) = value.get_mut("providers").and_then(|v| v.as_object_mut()) {
        for provider in providers.values_mut() {
            if provider["type"] == "glm-coding-plan" {
                provider["type"] = serde_json::json!("response");
                if provider.get("base_url").is_none() {
                    provider["base_url"] = serde_json::json!(super::GLM_CODING_PLAN_BASE_URL);
                }
                let models = provider
                    .as_object_mut()
                    .unwrap()
                    .entry("models")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(models) = models.as_object_mut() {
                    for (id, model) in &super::model::glm_coding_plan_preset().models {
                        models
                            .entry(id.clone())
                            .or_insert(serde_json::to_value(model)?);
                    }
                }
            }
        }
    }
    let mut config: AppConfig = serde_json::from_value(value)?;
    config.hydrate_builtin_providers();
    Ok(config)
}

/// 从文件路径加载配置
pub fn load_config_file(path: &Path) -> Result<AppConfig, ConfigError> {
    let content = fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    parse_config_str(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_backends_resolve_endpoints_and_preserve_overrides() {
        use crate::config::DecisionsProviderType;

        for (provider, backend, expected_type) in [
            ("decision-key", None, DecisionsProviderType::OpenRouter),
            ("openrouter", None, DecisionsProviderType::OpenRouter),
            ("opencode", None, DecisionsProviderType::OpenCode),
            (
                "decision-key",
                Some("opencode"),
                DecisionsProviderType::OpenCode,
            ),
            (
                "openrouter",
                Some("opencode"),
                DecisionsProviderType::OpenCode,
            ),
            (
                "opencode",
                Some("openrouter"),
                DecisionsProviderType::OpenRouter,
            ),
        ] {
            let expected_endpoint = match expected_type {
                DecisionsProviderType::OpenRouter => "https://openrouter.ai/api/alpha/decisions",
                DecisionsProviderType::OpenCode => {
                    "https://opencode.ai/inference/systemone/v1/systemone"
                }
            };
            for endpoint in [None, Some("https://gateway.example/v1/systemone")] {
                let mut decisions = serde_json::json!({
                    "provider": provider,
                    "model": if expected_type == DecisionsProviderType::OpenCode { "jev-1.13-free" } else { "typesafe/jev-1.13" }
                });
                if let Some(backend) = backend {
                    decisions["type"] = serde_json::json!(backend);
                }
                if let Some(endpoint) = endpoint {
                    decisions["endpoint"] = serde_json::json!(endpoint);
                }
                let config = serde_json::json!({
                    "settings": {"agent": {"router": {"decisions": decisions}}},
                    "providers": {(provider): {"api_key": "test-key"}}
                });
                let config = parse_config_str(&config.to_string()).unwrap();
                let agent = config.settings.agent.unwrap();
                agent.validate().unwrap();
                let decisions = agent.router.decisions.unwrap();
                assert_eq!(decisions.effective_type(), expected_type);
                assert_eq!(decisions.endpoint(), endpoint.unwrap_or(expected_endpoint));
                assert_eq!(decisions.provider, provider);
                assert_eq!(decisions.min_confidence, 0.6);
            }
        }
    }

    #[test]
    fn decisions_reject_unknown_backend() {
        assert!(
            parse_config_str(
                r#"{"settings":{"agent":{"router":{"decisions":{
                "type":"unknown", "provider":"decision-key", "model":"jev-1.13"
            }}}}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn test_strip_jsonc_comments() {
        let jsonc = r#"{
            // line comment
            "key": "value // not a comment",
            /* block
               comment */
            "nested": {
                "num": 42
            }
        }"#;

        let clean = strip_jsonc_comments(jsonc);
        assert!(!clean.contains("// line comment"));
        assert!(!clean.contains("block"));
        assert!(clean.contains(r#""value // not a comment""#));
        assert!(clean.contains(r#""num": 42"#));

        let parsed: serde_json::Value = serde_json::from_str(&clean).expect("valid json");
        assert_eq!(parsed["key"], "value // not a comment");
        assert_eq!(parsed["nested"]["num"], 42);
    }

    #[test]
    fn test_interpolate_env_vars() {
        unsafe {
            std::env::set_var("VVA_TEST_API_KEY", "sk-123456789");
        }
        let input = r#"{"api_key": "${VVA_TEST_API_KEY}", "missing": "${NON_EXISTENT_XYZ}"}"#;
        let result = interpolate_env_vars(input);
        assert!(result.contains(r#""api_key": "sk-123456789""#));
        assert!(result.contains(r#""missing": """#));
    }
}
