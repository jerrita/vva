use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Response,
    Anthropic,
    Gemini,
}

impl Default for ProviderType {
    fn default() -> Self {
        Self::Response
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub settings: GlobalSettings,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

impl AppConfig {
    /// 合并配置：以 self 为基础（全局配置），叠加 project（项目配置），项目配置优先级高于全局配置
    pub fn merge(mut self, project: AppConfig) -> Self {
        if !project.settings.default.is_empty() {
            self.settings.default = project.settings.default;
        }
        if project.settings.small_model.is_some() {
            self.settings.small_model = project.settings.small_model;
        }

        for (p_id, p_cfg) in project.providers {
            if let Some(base_prov) = self.providers.get_mut(&p_id) {
                if !p_cfg.base_url.is_empty() {
                    base_prov.base_url = p_cfg.base_url;
                }
                if !p_cfg.api_key.is_empty() {
                    base_prov.api_key = p_cfg.api_key;
                }
                base_prov.provider_type = p_cfg.provider_type;
                for (m_id, m_cfg) in p_cfg.models {
                    base_prov.models.insert(m_id, m_cfg);
                }
            } else {
                self.providers.insert(p_id, p_cfg);
            }
        }

        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalSettings {
    #[serde(default)]
    pub default: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type", default)]
    pub provider_type: ProviderType,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    /// 可选的模型级供应商协议覆写（例如在 response 代理节点下声明 anthropic 原生协议）
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub model_type_override: Option<ProviderType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modalities: Option<ModalitiesConfig>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<ModelLimitConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCostConfig>,
}

impl ModelConfig {
    /// 计算该模型的实际有效供应商类型
    pub fn effective_type(&self, provider_type: ProviderType) -> ProviderType {
        self.model_type_override.unwrap_or(provider_type)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModalitiesConfig {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLimitConfig {
    pub context: usize,
    pub output: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCostConfig {
    pub input: f64,
    pub output: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_over_200k: Option<ContextOver200kCost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOver200kCost {
    pub input: f64,
    pub output: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

