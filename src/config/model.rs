use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// GLM Coding Plan 供应商节点 id；配置中仅填 `api_key` 时按此 id 识别并自动补全。
pub const GLM_CODING_PLAN_PROVIDER_ID: &str = "glm-coding-plan";
/// Codex / Responses 原生端点，支持 `web_search`。
pub const GLM_CODING_PLAN_BASE_URL: &str = "https://open.bigmodel.cn/api/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ProviderType {
    #[default]
    Response,
    Anthropic,
    Gemini,
}

impl ProviderType {
    pub fn require_media(
        self,
        kind: crate::model::MediaKind,
        mime_type: &str,
    ) -> Result<(), String> {
        use crate::model::MediaKind;
        let supported = match (self, kind) {
            (Self::Gemini, _) => true,
            (_, MediaKind::Pdf) => mime_type == "application/pdf",
            (_, MediaKind::Image) => matches!(
                mime_type,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ),
            _ => false,
        };
        if supported {
            Ok(())
        } else {
            Err(format!(
                "Protocol '{}' does not support {mime_type} input",
                self.as_str()
            ))
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Response => "response",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
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
                // 仅在项目配置显式声明 type 时覆写，防止 serde 默认值静默翻转协议
                if let Some(provider_type) = p_cfg.provider_type {
                    base_prov.provider_type = Some(provider_type);
                }
                for (m_id, m_cfg) in p_cfg.models {
                    base_prov.models.insert(m_id, m_cfg);
                }
            } else {
                self.providers.insert(p_id, p_cfg);
            }
        }

        self.hydrate_builtin_providers();
        self
    }

    /// 为内置供应商预设补全 type / base_url / 模型清单。已显式声明的字段与模型不被覆盖。
    pub fn hydrate_builtin_providers(&mut self) {
        for (id, cfg) in self.providers.iter_mut() {
            if !is_glm_coding_plan_provider(id, cfg.provider_type) {
                continue;
            }
            cfg.provider_type = Some(ProviderType::Response);
            if cfg.base_url.is_empty() {
                cfg.base_url = GLM_CODING_PLAN_BASE_URL.to_string();
            }
            for (model_id, model_cfg) in glm_coding_plan_builtin_models() {
                cfg.models.entry(model_id).or_insert(model_cfg);
            }
        }
    }
}

fn is_glm_coding_plan_provider(id: &str, provider_type: Option<ProviderType>) -> bool {
    id == GLM_CODING_PLAN_PROVIDER_ID
        && matches!(provider_type, Some(ProviderType::Response) | None)
}

fn glm_coding_plan_builtin_models() -> HashMap<String, ModelConfig> {
    let mut models = HashMap::new();
    models.insert(
        "glm-5.3".into(),
        ModelConfig {
            name: "GLM 5.3".into(),
            model_type_override: None,
            modalities: Some(ModalitiesConfig {
                input: vec!["text".into()],
                output: Vec::new(),
            }),
            reasoning: true,
            limit: Some(ModelLimitConfig {
                context: 1_000_000,
                output: 128_000,
            }),
            variants: Some(vec!["low".into(), "high".into(), "max".into()]),
            cost: Some(ModelCostConfig {
                input: 1.4,
                output: 4.4,
                cache_read: Some(0.26),
                cache_write: None,
                context_over_200k: None,
            }),
            annotation: None,
        },
    );
    models.insert(
        "glm-5.3-flash".into(),
        ModelConfig {
            name: "GLM 5.3 Flash".into(),
            model_type_override: None,
            modalities: Some(ModalitiesConfig {
                input: vec!["text".into(), "image".into(), "video".into(), "pdf".into()],
                output: Vec::new(),
            }),
            reasoning: true,
            limit: Some(ModelLimitConfig {
                context: 1_000_000,
                output: 128_000,
            }),
            variants: Some(vec!["low".into(), "high".into(), "max".into()]),
            cost: Some(ModelCostConfig {
                input: 0.15,
                output: 0.5,
                cache_read: Some(0.03),
                cache_write: None,
                context_over_200k: None,
            }),
            annotation: None,
        },
    );
    models
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
    /// 供应商协议类型；`None` 表示配置未显式声明 `type` 字段，
    /// 合并时不覆写既有声明，解析时按 Response 协议处理
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub provider_type: Option<ProviderType>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<ModelAnnotation>,
}

impl ModelConfig {
    /// 计算该模型的实际有效供应商类型
    pub fn effective_type(&self, provider_type: ProviderType) -> ProviderType {
        self.model_type_override.unwrap_or(provider_type)
    }

    /// 是否向 API 声明 web_fetch 服务端工具（`annotation.web_fetch: false` 时关闭）
    pub fn declare_web_fetch(&self) -> bool {
        self.annotation
            .as_ref()
            .and_then(|a| a.web_fetch)
            .unwrap_or(true)
    }

    /// 该模型的文件编辑协议；缺省为 `hashline`
    pub fn editor(&self) -> crate::model::EditorKind {
        self.annotation
            .as_ref()
            .and_then(|a| a.editor)
            .unwrap_or_default()
    }

    /// 该模型的请求服务档位；`annotation.fast: true` 时以 `service_tier: "fast"` 发起请求
    pub fn service_tier(&self) -> Option<&'static str> {
        match self.annotation.as_ref().and_then(|a| a.fast) {
            Some(true) => Some("fast"),
            _ => None,
        }
    }
}

/// 模型级注记：细粒度关闭个别原生服务端工具的声明，以及文件编辑协议。
/// 用于未接线特定 server tool 的中转端点——带未识别条目时端点会整组丢弃工具声明，
/// 例如 `annotation: { "web_fetch": false }` 仅声明 web_search 以保住原生搜索
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAnnotation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<bool>,
    /// 文件编辑协议：`hashline`（默认，`[PATH#TAG]` 行号补丁）、`replace`（old_string/new_string）、`patch`（Codex V4A）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<crate::model::EditorKind>,
    /// 以 `"service_tier": "fast"` 发起请求（快速档位端点）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
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

impl ModelCostConfig {
    pub fn estimate_usd(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_write: u64,
        context_tokens: u64,
    ) -> f64 {
        let (input, output, cache_read_rate, cache_write_rate) = if context_tokens > 200_000
            && let Some(over) = &self.context_over_200k
        {
            (
                over.input,
                over.output,
                over.cache_read.or(self.cache_read).unwrap_or(0.0),
                over.cache_write.or(self.cache_write).unwrap_or(0.0),
            )
        } else {
            (
                self.input,
                self.output,
                self.cache_read.unwrap_or(0.0),
                self.cache_write.unwrap_or(0.0),
            )
        };
        const PER_MILLION: f64 = 1_000_000.0;
        (input_tokens as f64) * (input / PER_MILLION)
            + (output_tokens as f64) * (output / PER_MILLION)
            + (cache_read as f64) * (cache_read_rate / PER_MILLION)
            + (cache_write as f64) * (cache_write_rate / PER_MILLION)
    }
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
