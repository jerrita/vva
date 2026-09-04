pub mod model;
pub mod parser;
pub mod resolver;

use std::path::{Path, PathBuf};

pub use model::{AppConfig, GlobalSettings, ModelConfig, ProviderConfig, ProviderType};
pub use parser::{ConfigError, load_config_file, parse_config_str, strip_jsonc_comments};
pub use resolver::ResolvedModel;

/// 获取当前用户的主目录路径（跨平台支持 HOME 与 Windows USERPROFILE / HOMEDRIVE+HOMEPATH）
pub fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME")
        && !home.trim().is_empty()
    {
        return Some(PathBuf::from(home));
    }
    if let Ok(profile) = std::env::var("USERPROFILE")
        && !profile.trim().is_empty()
    {
        return Some(PathBuf::from(profile));
    }
    if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        let combined = format!("{}{}", drive, path);
        if !combined.trim().is_empty() {
            return Some(PathBuf::from(combined));
        }
    }
    None
}

/// 展开路径中的 `~` 前缀为用户主目录
pub fn expand_tilde<P: AsRef<Path>>(path: P) -> PathBuf {
    let path = path.as_ref();
    if let Ok(stripped) = path.strip_prefix("~")
        && let Some(home) = home_dir()
    {
        return home.join(stripped);
    }
    path.to_path_buf()
}

/// 自动寻址并合并加载全局配置与项目配置（支持 JSON 和 JSONC，项目配置优先级高于全局配置）
pub fn load_config(custom_path: Option<&Path>) -> Result<AppConfig, ConfigError> {
    load_config_with_workspace(custom_path, None)
}

/// 支持指定 workspace 路径的配置加载与合并（项目配置覆盖全局配置）
pub fn load_config_with_workspace(
    custom_path: Option<&Path>,
    workspace: Option<&Path>,
) -> Result<AppConfig, ConfigError> {
    // 1. 加载全局配置 (~/.vva/config.jsonc 或 ~/.vva/config.json)
    let global_config = {
        let mut global_paths = Vec::new();
        if let Some(home) = home_dir() {
            let home_vva = home.join(".vva");
            global_paths.push(home_vva.join("config.jsonc"));
            global_paths.push(home_vva.join("config.json"));
        }
        let mut loaded = None;
        for gp in &global_paths {
            if gp.exists() {
                loaded = Some(load_config_file(gp)?);
                break;
            }
        }
        loaded
    };

    // 2. 加载项目配置 (custom_path, VVA_CONFIG, 或 workspace/cwd 目录下的 vva.jsonc/config.jsonc 等)
    let project_config = {
        if let Some(p) = custom_path {
            let p = expand_tilde(p);
            Some(load_config_file(&p)?)
        } else if let Ok(env_path) = std::env::var("VVA_CONFIG") {
            let p = expand_tilde(Path::new(&env_path));
            if p.exists() {
                Some(load_config_file(&p)?)
            } else {
                return Err(ConfigError::NotFound(vec![p.display().to_string()]));
            }
        } else {
            let base_dir = workspace
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

            let candidate_filenames = [
                "vva.jsonc",
                "vva.json",
                ".vva/config.jsonc",
                ".vva/config.json",
            ];

            let mut loaded = None;
            for name in &candidate_filenames {
                let p = base_dir.join(name);
                if p.exists() {
                    loaded = Some(load_config_file(&p)?);
                    break;
                }
            }
            loaded
        }
    };

    // 3. 合并计算：项目大于全局
    match (global_config, project_config) {
        (Some(global), Some(project)) => Ok(global.merge(project)),
        (None, Some(project)) => Ok(project),
        (Some(global), None) => Ok(global),
        (None, None) => {
            let mut searched = Vec::new();
            if let Some(home) = home_dir() {
                let home_vva = home.join(".vva");
                searched.push(home_vva.join("config.jsonc").display().to_string());
                searched.push(home_vva.join("config.json").display().to_string());
            }
            searched.push("vva.jsonc".to_string());
            searched.push("config.jsonc".to_string());
            Err(ConfigError::NotFound(searched))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_merge_project_over_global() {
        let global_json = r#"{
            "settings": {
                "default": "prov1/model-a",
                "small_model": "prov1/model-s"
            },
            "providers": {
                "prov1": {
                    "type": "response",
                    "base_url": "https://global.example.com",
                    "api_key": "global-key",
                    "models": {
                        "model-a": { "name": "Model A Global" },
                        "model-b": { "name": "Model B Global" }
                    }
                }
            }
        }"#;

        let project_json = r#"{
            "settings": {
                "default": "prov2/model-c"
            },
            "providers": {
                "prov1": {
                    "base_url": "https://project.example.com",
                    "models": {
                        "model-a": { "name": "Model A Project Overridden" },
                        "model-x": { "name": "Model X Added" }
                    }
                },
                "prov2": {
                    "type": "anthropic",
                    "base_url": "https://anthropic.example.com",
                    "api_key": "proj-key-2",
                    "models": {
                        "model-c": { "name": "Model C" }
                    }
                }
            }
        }"#;

        let global = parse_config_str(global_json).expect("parse global");
        let project = parse_config_str(project_json).expect("parse project");

        let merged = global.merge(project);

        // 验证 settings：项目优先，未配置项保留全局
        assert_eq!(merged.settings.default, "prov2/model-c");
        assert_eq!(
            merged.settings.small_model.as_deref(),
            Some("prov1/model-s")
        );

        // 验证 prov1：URL 覆盖，API key 保留全局，模型合并
        let p1 = merged.providers.get("prov1").expect("prov1 exists");
        assert_eq!(p1.base_url, "https://project.example.com");
        assert_eq!(p1.api_key, "global-key");
        assert_eq!(p1.models.len(), 3);
        assert_eq!(p1.models["model-a"].name, "Model A Project Overridden");
        assert_eq!(p1.models["model-b"].name, "Model B Global");
        assert_eq!(p1.models["model-x"].name, "Model X Added");

        // 验证 prov2：新增 provider
        let p2 = merged.providers.get("prov2").expect("prov2 exists");
        assert_eq!(p2.provider_type, ProviderType::Anthropic);
        assert_eq!(p2.base_url, "https://anthropic.example.com");
        assert_eq!(p2.api_key, "proj-key-2");
        assert_eq!(p2.models["model-c"].name, "Model C");
    }

    #[test]
    fn test_load_example_config_jsonc() {
        let path = Path::new("config.example.jsonc");
        if !path.exists() {
            return;
        }

        let config = load_config_file(path).expect("parse config.example.jsonc");
        assert_eq!(config.settings.default, "a/gpt-5.6-terra");
        assert_eq!(
            config.settings.small_model.as_deref(),
            Some("a/gpt-5.6-luna")
        );

        let provider_a = config.providers.get("a").expect("provider a exists");
        assert_eq!(provider_a.provider_type, ProviderType::Response);

        // 1. 验证默认模型解析与 Variant 校验
        let resolved = config
            .resolve_model("a/gpt-5.6-terra", Some("high"))
            .expect("resolve terra");
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
        let err = config
            .resolve_model("a/gpt-5.6-terra", Some("super-ultra-max"))
            .unwrap_err();
        match err {
            ConfigError::InvalidVariant { variant, .. } => {
                assert_eq!(variant, "super-ultra-max");
            }
            _ => panic!("Expected InvalidVariant error"),
        }

        // 4. 验证 gpt-5.6-luna 解析及其 variants (包含 medium)
        let resolved_luna = config
            .resolve_model("a/gpt-5.6-luna", Some("medium"))
            .expect("resolve luna");
        assert_eq!(resolved_luna.selected_variant.as_deref(), Some("medium"));
        assert_eq!(resolved_luna.provider.name(), "response");
    }

    #[test]
    fn test_load_config_with_workspace_does_not_load_example() {
        let temp_ws = std::env::temp_dir().join(format!("vva_ws_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_ws);
        let example_path = temp_ws.join("config.example.jsonc");
        std::fs::write(
            &example_path,
            r#"{"settings":{"default":"example/model"},"providers":{}}"#,
        )
        .unwrap();

        let res = load_config_with_workspace(None, Some(&temp_ws));
        if let Ok(cfg) = res {
            assert_ne!(cfg.settings.default, "example/model");
        }

        let _ = std::fs::remove_dir_all(&temp_ws);
    }

    #[test]
    fn test_home_dir_resolution() {
        let home = home_dir();
        assert!(home.is_some());
    }

    #[test]
    fn test_expand_tilde() {
        if let Some(home) = home_dir() {
            let path = Path::new("~/.vva/config.jsonc");
            let expanded = expand_tilde(path);
            assert_eq!(expanded, home.join(".vva").join("config.jsonc"));
        }
        let rel_path = Path::new("relative/config.json");
        assert_eq!(expand_tilde(rel_path), rel_path);
    }
}
