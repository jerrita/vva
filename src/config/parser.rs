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
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;

    while i < len {
        let ch = chars[i];

        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
        } else {
            if ch == '"' {
                in_string = true;
                escaped = false;
                out.push(ch);
                i += 1;
            } else if ch == '/' && i + 1 < len && chars[i + 1] == '/' {
                // 单行注释：跳至行尾，保留换行符以维持报错行号
                i += 2;
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            } else if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
                // 块级注释：跳至结束符 */
                i += 2;
                while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                    if chars[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
                if i + 1 < len {
                    i += 2; // 跳过 */
                }
            } else {
                out.push(ch);
                i += 1;
            }
        }
    }

    out
}

/// 展开文本中的 `${ENV_VAR}` 环境变量
pub fn interpolate_env_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '$' && i + 1 < len && chars[i + 1] == '{' {
            let start = i + 2;
            let mut end = start;
            while end < len && chars[end] != '}' {
                end += 1;
            }
            if end < len {
                let var_name: String = chars[start..end].iter().collect();
                let var_val = std::env::var(&var_name).unwrap_or_default();
                out.push_str(&var_val);
                i = end + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }

    out
}

/// 解析 JSON 或 JSONC 字符串（同时支持注释剔除与环境变量展开）
pub fn parse_config_str(input: &str) -> Result<AppConfig, ConfigError> {
    let stripped = strip_jsonc_comments(input);
    let interpolated = interpolate_env_vars(&stripped);
    let mut config: AppConfig = serde_json::from_str(&interpolated)?;
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
