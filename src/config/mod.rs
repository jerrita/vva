pub mod model;
pub mod parser;
pub mod resolver;

use std::path::{Path, PathBuf};

pub use model::{AppConfig, GlobalSettings, ModelConfig, ProviderConfig, ProviderType};
pub use parser::{load_config_file, parse_config_str, strip_jsonc_comments, ConfigError};
pub use resolver::ResolvedModel;

/// 自动寻址并加载配置（支持 JSON 和 JSONC）
pub fn load_config(custom_path: Option<&Path>) -> Result<AppConfig, ConfigError> {
    if let Some(p) = custom_path {
        return load_config_file(p);
    }

    if let Ok(env_path) = std::env::var("VVA_CONFIG") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return load_config_file(&p);
        }
    }

    let mut search_paths = Vec::new();

    // 当前工作目录
    search_paths.push(PathBuf::from("vva.jsonc"));
    search_paths.push(PathBuf::from("vva.json"));
    search_paths.push(PathBuf::from("config.jsonc"));
    search_paths.push(PathBuf::from("config.json"));
    search_paths.push(PathBuf::from("config.example.jsonc"));

    // 用户主目录 ~/.vva/
    if let Ok(home) = std::env::var("HOME") {
        let home_vva = PathBuf::from(home).join(".vva");
        search_paths.push(home_vva.join("config.jsonc"));
        search_paths.push(home_vva.join("config.json"));
    }

    for p in &search_paths {
        if p.exists() {
            return load_config_file(p);
        }
    }

    Err(ConfigError::NotFound(
        search_paths.iter().map(|p| p.display().to_string()).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_example_config_jsonc() {
        let path = Path::new("config.example.jsonc");
        if !path.exists() {
            return;
        }

        let config = load_config_file(path).expect("parse config.example.jsonc");
        assert_eq!(config.settings.default, "a/gpt-5.6-terra");
        assert_eq!(config.settings.small_model.as_deref(), Some("a/gpt-5.6-luna"));

        let provider_a = config.providers.get("a").expect("provider a exists");
        assert_eq!(provider_a.provider_type, ProviderType::Response);

        // 1. 验证默认模型解析与 Variant 校验
        let resolved = config.resolve_model("a/gpt-5.6-terra", Some("high")).expect("resolve terra");
        assert_eq!(resolved.provider_id, "a");
        assert_eq!(resolved.model_id, "gpt-5.6-terra");
        assert_eq!(resolved.selected_variant.as_deref(), Some("high"));
        assert_eq!(resolved.provider.name(), "response");

        // 2. 验证模型级协议覆写（Claude 在 provider a 下覆写为 Anthropic）
        let resolved_claude = config
            .resolve_model("a/claude-sonnet-4-6", None)
            .expect("resolve claude");
        assert_eq!(resolved_claude.provider.name(), "anthropic");
        // variants 中包含 medium，默认选择 medium
        assert_eq!(resolved_claude.selected_variant.as_deref(), Some("medium"));

        // 3. 验证非法 Variant 拦截
        let err = config.resolve_model("a/gpt-5.6-terra", Some("super-ultra-max")).unwrap_err();
        match err {
            ConfigError::InvalidVariant { variant, .. } => {
                assert_eq!(variant, "super-ultra-max");
            }
            _ => panic!("Expected InvalidVariant error"),
        }
    }
}

