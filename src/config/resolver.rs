use std::sync::Arc;

use super::model::{AppConfig, ModelConfig, ProviderConfig, ProviderType};
use super::parser::ConfigError;
use crate::provider::Provider;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::gemini::GeminiProvider;
use crate::provider::response::ResponseProvider;

pub struct ResolvedModel<'a> {
    pub provider_id: String,
    pub model_id: String,
    pub full_spec: String,
    pub model_config: ModelConfig,
    provider_config: &'a ProviderConfig,
    pub selected_variant: Option<String>,
}

impl std::fmt::Debug for ResolvedModel<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedModel")
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .field("full_spec", &self.full_spec)
            .field("model_config", &self.model_config)
            .field("protocol", &self.provider_config.provider_type)
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
    ) -> Result<ResolvedModel<'_>, ConfigError> {
        let (provider_id, model_id) = if let Some((p, m)) = spec.split_once('/') {
            (p.to_string(), m.to_string())
        } else {
            let mut matches = self
                .providers
                .iter()
                .filter(|(_, provider)| provider.models.contains_key(spec));
            if let Some((id, _)) = matches.next() {
                if matches.next().is_some() {
                    return Err(ConfigError::ModelNotFound(format!(
                        "Ambiguous model '{spec}'; use provider/model"
                    )));
                }
                (id.clone(), spec.to_string())
            } else {
                let (id, _) = self
                    .settings
                    .default
                    .split_once('/')
                    .ok_or_else(|| ConfigError::ModelNotFound(spec.to_string()))?;
                if !self.providers.get(id).is_some_and(|p| p.models.is_empty()) {
                    return Err(ConfigError::ModelNotFound(spec.to_string()));
                }
                (id.to_string(), spec.to_string())
            }
        };

        let provider_cfg = self.providers.get(&provider_id).ok_or_else(|| {
            ConfigError::ModelNotFound(format!("Provider '{provider_id}' not found"))
        })?;

        let model_cfg = if provider_cfg.models.is_empty() {
            ModelConfig {
                reasoning: true,
                ..Default::default()
            }
        } else {
            provider_cfg.models.get(&model_id).cloned().ok_or_else(|| {
                ConfigError::ModelNotFound(format!(
                    "Model '{model_id}' not found under provider '{provider_id}'"
                ))
            })?
        };

        // 校验或确定推理强度档位 (Variant)
        let selected_variant = if let Some(req_var) = requested_variant {
            if req_var == "none"
                && !model_cfg
                    .variants
                    .as_ref()
                    .is_some_and(|variants| variants.iter().any(|variant| variant == req_var))
            {
                return Err(ConfigError::InvalidVariant {
                    variant: req_var.to_string(),
                    model: model_id.clone(),
                    valid: model_cfg.variants.clone().unwrap_or_default(),
                });
            }
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
        } else {
            None
        };

        let full_spec = format!("{provider_id}/{model_id}");
        Ok(ResolvedModel {
            provider_id,
            model_id,
            full_spec,
            model_config: model_cfg,
            provider_config: provider_cfg,
            selected_variant,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct InputCapabilities {
    pub model: String,
    pub modalities: Vec<String>,
    pub protocol: ProviderType,
}

impl InputCapabilities {
    pub fn require(&self, kind: crate::model::MediaKind, mime_type: &str) -> Result<(), String> {
        if !self.modalities.iter().any(|input| input == kind.as_str()) {
            return Err(format!(
                "Model '{}' does not support {} input",
                self.model,
                kind.as_str()
            ));
        }
        self.protocol.require_media(kind, mime_type)
    }

    pub fn validate_session(&self, session: &crate::model::Session) -> Result<(), String> {
        for part in session
            .turns
            .iter()
            .flat_map(|turn| &turn.messages)
            .flat_map(|msg| &msg.parts)
        {
            match part {
                crate::model::ContentPart::Image { source, mime_type } => {
                    if !self.modalities.iter().any(|input| input == "image") {
                        return Err(format!(
                            "Model '{}' does not support image input",
                            self.model
                        ));
                    }
                    let mime = match source {
                        crate::model::MediaSource::Base64 { mime_type, .. } => {
                            Some(mime_type.as_str())
                        }
                        crate::model::MediaSource::Url { .. } => mime_type.as_deref(),
                    };
                    if let Some(mime) = mime {
                        self.protocol
                            .require_media(crate::model::MediaKind::Image, mime)?;
                    }
                }
                crate::model::ContentPart::ToolResult(result) => {
                    for media in &result.attachments {
                        self.require(media.kind, &media.mime_type)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl ResolvedModel<'_> {
    pub fn input_capabilities(&self) -> InputCapabilities {
        InputCapabilities {
            model: self.full_spec.clone(),
            modalities: self
                .model_config
                .modalities
                .as_ref()
                .map(|m| m.input.clone())
                .unwrap_or_default(),
            protocol: self
                .model_config
                .effective_type(self.provider_config.provider_type.unwrap_or_default()),
        }
    }

    pub fn create_provider(&self) -> Arc<dyn Provider> {
        // 计算实际生效的 ProviderType（支持模型级 override；未声明 type 时按 Response）
        let effective_type = self
            .model_config
            .effective_type(self.provider_config.provider_type.unwrap_or_default());

        // 规范化 Base URL（针对各 API 标准端点严格对接与去重）
        let base_url = self.provider_config.base_url.trim().trim_end_matches('/');
        let api_key = self.provider_config.api_key.trim();

        let provider_inst: Arc<dyn Provider> = match effective_type {
            ProviderType::Response => {
                let default_url = "https://api.openai.com/v1";
                let url = if base_url.is_empty() {
                    default_url.to_string()
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

        provider_inst
    }
}
