use std::sync::Arc;

use super::model::{AppConfig, ModelConfig, ProviderType};
use super::parser::ConfigError;
use crate::provider::Provider;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::gemini::GeminiProvider;
use crate::provider::response::ResponseProvider;

pub struct ResolvedModel {
    pub provider_id: String,
    pub model_id: String,
    pub full_spec: String,
    pub model_config: ModelConfig,
    pub provider: Arc<dyn Provider>,
    pub selected_variant: Option<String>,
}

impl std::fmt::Debug for ResolvedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedModel")
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .field("full_spec", &self.full_spec)
            .field("model_config", &self.model_config)
            .field("provider_name", &self.provider.name())
            .field("selected_variant", &self.selected_variant)
            .finish()
    }
}

impl AppConfig {
    /// 根据模型规格标识（如 "a/gpt-5.6-terra"）解析出对应的 Provider 与配置
    pub fn resolve_model(
        &self,
        spec: &str,
        requested_variant: Option<&str>,
    ) -> Result<ResolvedModel, ConfigError> {
        let (provider_id, model_id) = if let Some((p, m)) = spec.split_once('/') {
            (p.to_string(), m.to_string())
        } else {
            // 若没有指定 provider 前缀，尝试从各 provider 中查找该 model
            let mut found = None;
            for (p_id, p_cfg) in &self.providers {
                if p_cfg.models.contains_key(spec) {
                    found = Some((p_id.clone(), spec.to_string()));
                    break;
                }
            }
            found.ok_or_else(|| ConfigError::ModelNotFound(spec.to_string()))?
        };

        let provider_cfg = self.providers.get(&provider_id).ok_or_else(|| {
            ConfigError::ModelNotFound(format!("Provider '{provider_id}' not found"))
        })?;

        let model_cfg = provider_cfg
            .models
            .get(&model_id)
            .ok_or_else(|| {
                ConfigError::ModelNotFound(format!(
                    "Model '{model_id}' not found under provider '{provider_id}'"
                ))
            })?
            .clone();

        // 校验或确定推理强度档位 (Variant)
        let selected_variant = if let Some(req_var) = requested_variant {
            if let Some(valid_vars) = &model_cfg.variants {
                if !valid_vars.iter().any(|v| v == req_var) {
                    return Err(ConfigError::InvalidVariant {
                        variant: req_var.to_string(),
                        model: model_id.clone(),
                        valid: valid_vars.clone(),
                    });
                }
                Some(req_var.to_string())
            } else if model_cfg.reasoning {
                Some(req_var.to_string())
            } else {
                None
            }
        } else if let Some(valid_vars) = &model_cfg.variants {
            // 默认优先选择 medium 或首选档位
            if valid_vars.iter().any(|v| v == "medium") {
                Some("medium".to_string())
            } else {
                valid_vars.first().cloned()
            }
        } else {
            None
        };

        // 计算实际生效的 ProviderType（支持模型级 override；未声明 type 时按 Response）
        let effective_type =
            model_cfg.effective_type(provider_cfg.provider_type.unwrap_or_default());

        // 规范化 Base URL（针对各 API 标准端点严格对接与去重）
        let base_url = provider_cfg.base_url.trim().trim_end_matches('/');
        let api_key = provider_cfg.api_key.trim();

        let provider_inst: Arc<dyn Provider> = match effective_type {
            ProviderType::Response => {
                let url = if base_url.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else if base_url.ends_with("/v1") || base_url.ends_with("/responses") {
                    base_url.to_string()
                } else {
                    format!("{base_url}/v1")
                };
                Arc::new(ResponseProvider::new(api_key).with_base_url(url))
            }
            ProviderType::Anthropic => {
                let url = if base_url.is_empty() {
                    "https://api.anthropic.com/v1".to_string()
                } else if base_url.ends_with("/v1") || base_url.ends_with("/messages") {
                    base_url.to_string()
                } else {
                    format!("{base_url}/v1")
                };
                Arc::new(AnthropicProvider::new(api_key).with_base_url(url))
            }
            ProviderType::Gemini => {
                let url = if base_url.is_empty() {
                    "https://generativelanguage.googleapis.com/v1beta".to_string()
                } else if base_url.ends_with("/v1beta")
                    || base_url.ends_with("/v1")
                    || base_url.contains("/models")
                {
                    base_url.to_string()
                } else {
                    format!("{base_url}/v1beta")
                };
                Arc::new(GeminiProvider::new(api_key).with_base_url(url))
            }
        };

        let full_spec = format!("{provider_id}/{model_id}");
        Ok(ResolvedModel {
            provider_id,
            model_id,
            full_spec,
            model_config: model_cfg,
            provider: provider_inst,
            selected_variant,
        })
    }
}

pub type ResolvedProviderInfo = (Arc<dyn Provider>, String, Option<String>);

/// 解析 Provider 实例、模型生效名称及 Variant。
/// 配置存在时走配置路由（历史会话模型失效时回退默认模型并记录告警）；
/// 无配置文件时按模型名与环境变量直连官方端点。
pub fn resolve_provider(
    app_config: &Option<AppConfig>,
    current_model: &str,
    current_variant: Option<&str>,
) -> Result<ResolvedProviderInfo, String> {
    if let Some(cfg) = app_config {
        match cfg.resolve_model(current_model, current_variant) {
            Ok(resolved) => {
                let ResolvedModel {
                    model_id,
                    provider,
                    selected_variant,
                    ..
                } = resolved;
                Ok((provider, model_id, selected_variant))
            }
            Err(err) => {
                // 如果历史会话中记录的模型在当前配置中不存在，回退到默认模型
                if current_model != cfg.settings.default
                    && let Ok(fallback) = cfg.resolve_model(&cfg.settings.default, None)
                {
                    tracing::warn!(
                        "Model '{current_model}' not found in current config; falling back to default '{}'",
                        cfg.settings.default
                    );
                    return Ok((
                        fallback.provider,
                        fallback.model_id,
                        fallback.selected_variant,
                    ));
                }
                Err(format!("Error resolving model '{current_model}': {err}"))
            }
        }
    } else {
        // 无配置文件时的环境变数备用路由
        let (provider_inst, model_id): (Arc<dyn Provider>, String) =
            match current_model.to_lowercase().as_str() {
                m if m.contains("anthropic") || m.contains("claude") => {
                    let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                    (
                        Arc::new(AnthropicProvider::new(key)),
                        current_model.to_string(),
                    )
                }
                m if m.contains("gemini") => {
                    let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
                    (
                        Arc::new(GeminiProvider::new(key)),
                        current_model.to_string(),
                    )
                }
                _ => {
                    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                    (
                        Arc::new(ResponseProvider::new(key)),
                        current_model.to_string(),
                    )
                }
            };
        Ok((provider_inst, model_id, current_variant.map(String::from)))
    }
}
