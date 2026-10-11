pub mod env;
pub mod model;
pub mod parser;
pub mod resolver;
pub mod state;

use std::path::{Path, PathBuf};

pub use model::{
    AgentChannels, AgentSpaceConfig, AppConfig, DecisionsRouterConfig, GLM_CODING_PLAN_BASE_URL,
    GLM_CODING_PLAN_PROVIDER_ID, GlobalSettings, ModelConfig, ProviderConfig, ProviderType,
    QqChannelConfig, RouterConfig,
};
pub use parser::{ConfigError, load_config_file, parse_config_str, strip_jsonc_comments};
pub use resolver::ResolvedModel;
pub use state::{AppState, runtime_state_path};

/// 无任何配置文件可用时的新会话兜底模型规格。
/// 必须是不带 provider 前缀的裸模型名，使环境变量路由（OPENAI_API_KEY 直连）仍可解析。
pub const DEFAULT_MODEL_SPEC: &str = "openai/gpt-5.6-terra";

/// 项目配置候选文件名（相对 base 目录按序尝试；NotFound 错误提示共用此列表）
const PROJECT_CONFIG_CANDIDATES: [&str; 4] = [
    "vva.jsonc",
    "vva.json",
    ".vva/config.jsonc",
    ".vva/config.json",
];

impl AppConfig {
    /// 将环境变量入口转换为与文件配置相同的供应商配置。
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.settings.default = DEFAULT_MODEL_SPEC.into();
        for (id, protocol, variable) in [
            ("openai", ProviderType::Response, "OPENAI_API_KEY"),
            ("anthropic", ProviderType::Anthropic, "ANTHROPIC_API_KEY"),
            ("gemini", ProviderType::Gemini, "GEMINI_API_KEY"),
        ] {
            config.providers.insert(
                id.into(),
                ProviderConfig {
                    provider_type: Some(protocol),
                    api_key: std::env::var(variable).unwrap_or_default(),
                    ..Default::default()
                },
            );
        }
        config
    }
}

/// 获取当前用户的主目录路径（跨平台支持 HOME 与 Windows USERPROFILE / HOMEDRIVE+HOMEPATH）
pub fn home_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    var("HOME")
        .or_else(|| var("USERPROFILE"))
        .or_else(|| Some(var("HOMEDRIVE")? + &var("HOMEPATH")?))
        .map(PathBuf::from)
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

/// 依次尝试候选路径，返回首个存在文件的配置；均不存在时为 `None`
fn load_first_config(paths: &[PathBuf]) -> Result<Option<AppConfig>, ConfigError> {
    for path in paths {
        if path.exists() {
            return Ok(Some(load_config_file(path)?));
        }
    }
    Ok(None)
}

/// 支持指定 workspace 路径的配置加载与合并（项目配置覆盖全局配置）
pub fn load_config_with_workspace(
    custom_path: Option<&Path>,
    workspace: Option<&Path>,
) -> Result<AppConfig, ConfigError> {
    // 1. 加载全局配置 (~/.vva/config.jsonc 或 ~/.vva/config.json)
    let global_paths: Vec<PathBuf> = home_dir()
        .map(|home| {
            let home_vva = home.join(".vva");
            vec![home_vva.join("config.jsonc"), home_vva.join("config.json")]
        })
        .unwrap_or_default();
    let global_config = load_first_config(&global_paths)?;

    // 2. 加载项目配置 (custom_path, VVA_CONFIG, 或 workspace/cwd 目录下的 vva.jsonc/vva.json/.vva/config.jsonc/.vva/config.json)
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
            let candidates: Vec<PathBuf> = PROJECT_CONFIG_CANDIDATES
                .iter()
                .map(|name| base_dir.join(name))
                .collect();
            load_first_config(&candidates)?
        }
    };

    // 3. 合并计算：项目大于全局
    match (global_config, project_config) {
        (Some(global), Some(project)) => Ok(global.merge(project)),
        (None, Some(project)) => Ok(project),
        (Some(global), None) => Ok(global),
        (None, None) => {
            let mut searched: Vec<String> = global_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            searched.extend(
                PROJECT_CONFIG_CANDIDATES
                    .iter()
                    .map(|name| name.to_string()),
            );
            Err(ConfigError::NotFound(searched))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_annotation_disables_web_fetch_declaration() {
        let json = r#"{
            "providers": {
                "prov": {
                    "type": "anthropic",
                    "models": {
                        "with-annotation": {
                            "name": "With Annotation",
                            "annotation": { "web_fetch": false }
                        },
                        "without-annotation": { "name": "Without Annotation" }
                    }
                }
            }
        }"#;
        let config = parse_config_str(json).expect("parse config");

        let annotated = &config.providers["prov"].models["with-annotation"];
        assert!(!annotated.declare_web_fetch());

        let plain = &config.providers["prov"].models["without-annotation"];
        assert!(plain.declare_web_fetch());
    }

    #[test]
    fn test_model_annotation_web_search_declaration() {
        let json = r#"{
            "providers": {
                "prov": {
                    "models": {
                        "plain": { "name": "Plain" },
                        "no-search": {
                            "name": "No Search",
                            "annotation": { "web_search": false }
                        }
                    }
                }
            }
        }"#;
        let config = parse_config_str(json).expect("parse config");

        assert!(
            !config.providers["prov"].models["no-search"].declare_web_search(),
            "annotation.web_search: false 应关闭原生搜索声明"
        );
        assert!(config.providers["prov"].models["plain"].declare_web_search());
    }

    #[test]
    fn test_model_annotation_affinity_default_and_override() {
        let json = r#"{
            "providers": {
                "prov": {
                    "models": {
                        "plain": { "name": "Plain" },
                        "gpt": {
                            "name": "GPT",
                            "annotation": { "affinity": "gpt" }
                        }
                    }
                }
            }
        }"#;
        let config = parse_config_str(json).expect("parse config");
        let models = &config.providers["prov"].models;

        // 缺省亲和组 = provider 节点 id；显式声明优先
        assert_eq!(models["plain"].affinity("prov"), "prov");
        assert_eq!(models["gpt"].affinity("prov"), "gpt");
    }

    #[test]
    fn test_settings_web_merge_replaces_section() {
        let global = parse_config_str(
            r#"{
            "settings": {
                "default": "a/x",
                "web": {
                    "provider": { "brave": { "api_key": "global-key" } },
                    "search": "brave",
                    "fetch": "local"
                }
            },
            "providers": { "a": { "models": { "x": { "name": "X" } } } }
        }"#,
        )
        .unwrap();
        assert_eq!(
            global.settings.web.as_ref().unwrap().search.as_deref(),
            Some("brave")
        );

        // 项目配置声明 web 时整段替换，不与全局逐字段合并
        let project = parse_config_str(
            r#"{
            "settings": {
                "web": { "provider": { "tavily": { "api_key": "project-key" } }, "search": "tavily" }
            }
        }"#,
        )
        .unwrap();
        let merged = global.merge(project);
        let web = merged.settings.web.as_ref().unwrap();
        assert_eq!(web.search.as_deref(), Some("tavily"));
        assert_eq!(web.fetch, None);
        let provider = web.provider.as_ref().unwrap();
        assert!(provider.brave.is_none());
        assert_eq!(provider.tavily.as_ref().unwrap().api_key, "project-key");
    }

    #[test]
    fn test_model_annotation_editor_override() {
        let json = r#"{
            "providers": {
                "prov": {
                    "type": "response",
                    "models": {
                        "plain": { "name": "Plain" },
                        "codex": { "name": "Codex", "annotation": { "editor": "patch" } },
                        "hashed": { "name": "Hashed", "annotation": { "editor": "hashline" } }
                    }
                }
            }
        }"#;
        let config = parse_config_str(json).expect("parse config");
        let models = &config.providers["prov"].models;
        assert_eq!(models["plain"].editor(), None);
        assert_eq!(
            models["codex"].editor(),
            Some(crate::model::EditorKind::Patch)
        );
        assert_eq!(
            models["hashed"].editor(),
            Some(crate::model::EditorKind::Hashline)
        );
    }

    #[test]
    fn test_settings_agent_qq_secret_missing_is_error() {
        let config = parse_config_str(
            r#"{ "settings": { "agent": { "channels": { "qq": { "app_id": "1", "secret": "${VVA_TEST_UNSET_QQ_SECRET}" } } } } }"#,
        )
        .expect("parse");
        let err = config.settings.agent.unwrap().validate().unwrap_err();
        assert!(err.contains("secret"));
    }

    #[test]
    fn test_settings_editor_merge() {
        let global =
            parse_config_str(r#"{ "settings": { "editor": "patch" } }"#).expect("parse global");
        assert_eq!(
            global.settings.editor,
            Some(crate::model::EditorKind::Patch)
        );

        // 项目未声明时保留全局值
        let merged = global
            .clone()
            .merge(parse_config_str(r#"{ "settings": {} }"#).expect("parse project"));
        assert_eq!(
            merged.settings.editor,
            Some(crate::model::EditorKind::Patch)
        );

        // 项目显式声明时覆写
        let merged = global.merge(
            parse_config_str(r#"{ "settings": { "editor": "replace" } }"#).expect("parse project"),
        );
        assert_eq!(
            merged.settings.editor,
            Some(crate::model::EditorKind::Replace)
        );
    }

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
        assert_eq!(p2.provider_type, Some(ProviderType::Anthropic));
        assert_eq!(p2.base_url, "https://anthropic.example.com");
        assert_eq!(p2.api_key, "proj-key-2");
        assert_eq!(p2.models["model-c"].name, "Model C");
    }

    #[test]
    fn test_config_merge_preserves_provider_type_when_omitted() {
        // 回归：项目配置对既有 provider 只覆写 base_url 而省略 type 时，
        // serde 默认值不得把全局的 anthropic 协议静默翻转为 response
        let global_json = r#"{
            "providers": {
                "prov": {
                    "type": "anthropic",
                    "base_url": "https://global.example.com",
                    "api_key": "key",
                    "models": { "m": { "name": "M" } }
                }
            }
        }"#;
        let project_json = r#"{
            "providers": {
                "prov": {
                    "base_url": "https://proxy.example.com",
                    "api_key": "project-key"
                }
            }
        }"#;

        let global = parse_config_str(global_json).expect("parse global");
        let project = parse_config_str(project_json).expect("parse project");
        let merged = global.merge(project);

        let p = merged.providers.get("prov").expect("prov exists");
        assert_eq!(p.provider_type, Some(ProviderType::Anthropic));
        assert_eq!(p.base_url, "https://proxy.example.com");
        assert_eq!(p.api_key, "project-key");

        // 显式声明时仍应覆写
        let project_override =
            parse_config_str(r#"{ "providers": { "prov": { "type": "gemini" } } }"#)
                .expect("parse override");
        let merged2 = parse_config_str(global_json)
            .expect("parse global 2")
            .merge(project_override);
        assert_eq!(
            merged2.providers.get("prov").unwrap().provider_type,
            Some(ProviderType::Gemini)
        );
    }

    #[test]
    fn test_parse_config_resolve_and_preset() {
        let json = r#"{
            "$schema": "https://example.com/vva/schema.json",
            "settings": {
                "default": "a/gpt-5.6-terra",
                "small_model": "a/gpt-5.6-luna"
            },
            "providers": {
                "a": {
                    "type": "response",
                    "models": {
                        "gpt-5.6-terra": {
                            "name": "GPT-5.6 Terra",
                            "reasoning": true,
                            "variants": ["minimal", "low", "medium", "high"]
                        },
                        "claude-sonnet-4-6": {
                            "name": "Claude Sonnet 4.6",
                            "type": "anthropic",
                            "reasoning": true
                        }
                    }
                },
                "glm-coding-plan": {
                    "api_key": "${ZAI_API_KEY}"
                }
            }
        }"#;
        let config = parse_config_str(json).expect("parse config");

        assert_eq!(config.settings.default, "a/gpt-5.6-terra");
        assert_eq!(
            config.settings.small_model.as_deref(),
            Some("a/gpt-5.6-luna")
        );

        // 1. 默认模型解析与 Variant 校验
        let resolved = config
            .resolve_model("a/gpt-5.6-terra", Some("high"))
            .expect("resolve terra");
        assert_eq!(resolved.provider_id, "a");
        assert_eq!(resolved.model_id, "gpt-5.6-terra");
        assert_eq!(resolved.selected_variant.as_deref(), Some("high"));
        assert_eq!(resolved.create_provider().name(), "response");

        // 2. 模型级协议覆写（Claude 在 response 供应商下覆写为 Anthropic）
        let resolved_claude = config
            .resolve_model("a/claude-sonnet-4-6", None)
            .expect("resolve claude");
        assert_eq!(resolved_claude.create_provider().name(), "anthropic");
        // 未指定档位时保留模型服务端的默认推理行为
        assert_eq!(resolved_claude.selected_variant, None);

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

        // glm-coding-plan 仅填 api_key 时补全端点与内置模型
        let glm = config
            .providers
            .get("glm-coding-plan")
            .expect("glm-coding-plan exists");
        assert_eq!(glm.base_url, GLM_CODING_PLAN_BASE_URL);
        let glm_resolved = config
            .resolve_model("glm-coding-plan/glm-5.3-flash", None)
            .expect("resolve glm-5.3-flash");
        assert_eq!(glm_resolved.create_provider().name(), "response");
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
