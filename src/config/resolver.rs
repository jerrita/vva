use std::sync::Arc;

use super::model::{AppConfig, ModelConfig, ProviderConfig, ProviderType};
use super::parser::ConfigError;
use crate::model::{ContentPart, MediaKind, MediaSource, Session};
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

        // 校验或确定推理强度档位 (Variant)：
        // 显式清单时逐项匹配；未声明清单时按 reasoning 开关决定是否透传
        let selected_variant = match (requested_variant, &model_cfg.variants) {
            (None, _) => None,
            (Some(req), Some(valid)) => {
                if !valid.iter().any(|v| v == req) {
                    return Err(ConfigError::InvalidVariant {
                        variant: req.to_string(),
                        model: model_id.clone(),
                        valid: valid.clone(),
                    });
                }
                Some(req.to_string())
            }
            // 未声明清单时 "none" 是保留值，其余档位仅在推理模型上透传
            (Some(req), None) => {
                if req == "none" {
                    return Err(ConfigError::InvalidVariant {
                        variant: req.to_string(),
                        model: model_id.clone(),
                        valid: Vec::new(),
                    });
                }
                model_cfg.reasoning.then(|| req.to_string())
            }
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
    pub fn require(&self, kind: MediaKind, mime_type: &str) -> Result<(), String> {
        if !self.modalities.iter().any(|input| input == kind.as_str()) {
            return Err(format!(
                "Model '{}' does not support {} input",
                self.model,
                kind.as_str()
            ));
        }
        self.protocol.require_media(kind, mime_type)
    }

    pub fn validate_session(&self, session: &Session) -> Result<(), String> {
        for part in session
            .turns
            .iter()
            .flat_map(|turn| &turn.messages)
            .flat_map(|msg| &msg.parts)
        {
            match part {
                ContentPart::Image { source, mime_type } => {
                    if !self.modalities.iter().any(|input| input == "image") {
                        return Err(format!(
                            "Model '{}' does not support image input",
                            self.model
                        ));
                    }
                    let mime = match source {
                        MediaSource::Base64 { mime_type, .. } => Some(mime_type.as_str()),
                        MediaSource::Url { .. } => mime_type.as_deref(),
                    };
                    if let Some(mime) = mime {
                        self.protocol.require_media(MediaKind::Image, mime)?;
                    }
                }
                ContentPart::File {
                    source: MediaSource::Base64 { mime_type, .. },
                    mime_type: declared_mime,
                    ..
                } => {
                    if declared_mime != mime_type {
                        return Err("File MIME type does not match its source".into());
                    }
                    self.require(MediaKind::Pdf, mime_type)?;
                }
                ContentPart::ToolResult(result) => {
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
    /// 模型亲和组：annotation.affinity 优先，缺省 provider 节点 id。
    /// 同组模型切换时可复用原生回传状态（链锚点 / 签名块），跨组降维重建
    pub fn affinity(&self) -> String {
        self.model_config.affinity(&self.provider_id)
    }

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

    /// 实际生效的 ProviderType（支持模型级 override；未声明 type 时按 Response）
    pub fn effective_type(&self) -> ProviderType {
        self.model_config
            .effective_type(self.provider_config.provider_type.unwrap_or_default())
    }

    pub fn create_provider(&self) -> Arc<dyn Provider> {
        let effective_type = self.effective_type();

        // 规范化 Base URL（针对各 API 标准端点严格对接与去重）
        let base_url = self.provider_config.base_url.trim().trim_end_matches('/');
        let api_key = self.provider_config.api_key.trim();

        // 空值取官方默认端点；已是完整端点（is_complete）原样使用，否则补版本段
        let endpoint = |default: &str, version: &str, is_complete: &dyn Fn(&str) -> bool| {
            if base_url.is_empty() {
                default.to_string()
            } else if is_complete(base_url) {
                base_url.to_string()
            } else {
                format!("{base_url}{version}")
            }
        };

        match effective_type {
            ProviderType::Response => {
                let url = endpoint("https://api.openai.com/v1", "/v1", &|u| {
                    u.ends_with("/v1") || u.ends_with("/responses")
                });
                Arc::new(ResponseProvider::new(api_key).with_base_url(url))
            }
            ProviderType::Anthropic => {
                let url = endpoint("https://api.anthropic.com/v1", "/v1", &|u| {
                    u.ends_with("/v1") || u.ends_with("/messages")
                });
                Arc::new(AnthropicProvider::new(api_key).with_base_url(url))
            }
            ProviderType::Gemini => {
                let url = endpoint(
                    "https://generativelanguage.googleapis.com/v1beta",
                    "/v1beta",
                    &|u| u.ends_with("/v1beta") || u.ends_with("/v1") || u.contains("/models"),
                );
                Arc::new(GeminiProvider::new(api_key).with_base_url(url))
            }
        }
    }
}
