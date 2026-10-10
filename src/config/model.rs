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
        let (base, over) = (&mut self.settings, project.settings);
        if !over.default.is_empty() {
            base.default = over.default;
        }
        base.small_model = over.small_model.or(base.small_model.take());
        base.editor = over.editor.or(base.editor);
        base.web = over.web.or(base.web.take());
        base.agent = over.agent.or(base.agent.take());

        for (p_id, p_cfg) in project.providers {
            self.providers.entry(p_id).or_default().merge(p_cfg);
        }

        self.hydrate_builtin_providers();
        self
    }

    /// 为内置供应商预设补全 type / base_url / 模型清单。已显式声明的字段与模型不被覆盖。
    pub fn hydrate_builtin_providers(&mut self) {
        let Some(cfg) = self.providers.get_mut(GLM_CODING_PLAN_PROVIDER_ID) else {
            return;
        };
        if !matches!(cfg.provider_type, Some(ProviderType::Response) | None) {
            return;
        }
        let preset = glm_coding_plan_preset();
        cfg.provider_type = preset.provider_type;
        if cfg.base_url.is_empty() {
            cfg.base_url = preset.base_url;
        }
        for (model_id, model_cfg) in preset.models {
            cfg.models.entry(model_id).or_insert(model_cfg);
        }
    }
}

/// glm-coding-plan 内置预设：Response 协议 + 官方端点 + 内置模型清单。
/// 配置解析 (parse_config_str) 与合并补全 (hydrate_builtin_providers) 共用此单一来源。
pub(crate) fn glm_coding_plan_preset() -> ProviderConfig {
    let mut models = HashMap::new();
    models.insert(
        "glm-5.3".into(),
        glm_builtin_model("GLM 5.3", &["text"], 1.4, 4.4, 0.26),
    );
    models.insert(
        "glm-5.3-flash".into(),
        glm_builtin_model(
            "GLM 5.3 Flash",
            &["text", "image", "video", "pdf"],
            0.15,
            0.5,
            0.03,
        ),
    );
    ProviderConfig {
        provider_type: Some(ProviderType::Response),
        base_url: GLM_CODING_PLAN_BASE_URL.to_string(),
        api_key: String::new(),
        models,
    }
}

/// 内置模型的公共骨架：推理开启、统一限额与档位，仅名称 / 输入模态 / 单价不同
fn glm_builtin_model(
    name: &str,
    input: &[&str],
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
) -> ModelConfig {
    ModelConfig {
        name: name.into(),
        model_type_override: None,
        modalities: Some(ModalitiesConfig {
            input: input.iter().map(|m| m.to_string()).collect(),
            output: Vec::new(),
        }),
        reasoning: true,
        limit: Some(ModelLimitConfig {
            context: 1_000_000,
            output: 128_000,
        }),
        variants: Some(vec!["low".into(), "high".into(), "max".into()]),
        cost: Some(ModelCostConfig {
            input: input_cost,
            output: output_cost,
            cache_read: Some(cache_read_cost),
            cache_write: None,
            context_over_200k: None,
        }),
        annotation: None,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalSettings {
    #[serde(default)]
    pub default: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,
    /// 模型未声明 `annotation.editor` 时的缺省文件编辑协议；缺省 `hashline`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<crate::model::EditorKind>,
    /// 外置 web search / fetch 补全配置；模型原生能力缺位时以客户端工具补齐
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web: Option<WebSettings>,
    /// agent space（常驻 IM agent）配置；`vva agent` 要求声明
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentSpaceConfig>,
}

/// agent space 配置：整数秒 / 整数 token；router 复用 `settings.small_model`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSpaceConfig {
    /// oneshot 与 topic session 使用的模型规格；缺省沿用 `settings.default`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 普通 session 空闲清理时长（秒）
    pub ttl_secs: u64,
    /// oneshot session 回复后的清理等待时长（秒）；0 表示回复完即可清理
    pub oneshot_ttl_secs: u64,
    /// rollover 预留线：上下文达到 `limit.context - 预留` 时回合结束触发
    pub rollover_reserve_tokens: u64,
    /// 待附件缓冲等待时长（秒），超时提示并清理
    pub pending_files_ttl_secs: u64,
    /// 定时任务单次执行的超时（秒），超时中止并计为失败
    pub task_timeout_secs: u64,
    /// 定时任务连续失败多少次后自动停用
    pub task_max_failures: u32,
    pub channels: AgentChannels,
}

impl Default for AgentSpaceConfig {
    fn default() -> Self {
        Self {
            model: None,
            ttl_secs: 18_000,
            oneshot_ttl_secs: 0,
            rollover_reserve_tokens: 32_000,
            pending_files_ttl_secs: 600,
            task_timeout_secs: 600,
            task_max_failures: 3,
            channels: AgentChannels::default(),
        }
    }
}

impl AgentSpaceConfig {
    /// 启动期校验：已声明的 channel 凭据不得为空
    pub fn validate(&self) -> Result<(), String> {
        if let Some(qq) = &self.channels.qq {
            if qq.app_id.is_empty() {
                return Err("settings.agent.channels.qq.app_id 为空".into());
            }
            if qq.secret.is_empty() {
                return Err(
                    "settings.agent.channels.qq.secret 为空（检查 ${VAR} 环境变量是否已设置）"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentChannels {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qq: Option<QqChannelConfig>,
}

/// QQ 官方机器人凭据与白名单；白名单外用户静默丢弃
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QqChannelConfig {
    #[serde(default)]
    pub app_id: String,
    /// 推荐 `${VAR}` 插值；环境变量缺失时展开为空，由 `validate` 显式报错
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

/// 外置 web 配置：`search` / `fetch` 按名称选择 provider。
/// `local` 为内置 fetch provider（无需声明），其余名称须在 `provider` 下声明。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<WebProviders>,
    /// web search 使用的 provider 名（`brave` / `tavily`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// web fetch 使用的 provider 名（`brave` / `local` / `tavily`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch: Option<String>,
}

/// 外置 web provider 声明；`local` 为内置保留名，不在此声明
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebProviders {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brave: Option<WebApiAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tavily: Option<WebApiAuth>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebApiAuth {
    #[serde(default)]
    pub api_key: String,
}

impl WebSettings {
    /// 声明了哪些 provider 名（不含内置 `local`）
    pub fn declared_names(&self) -> Vec<&str> {
        let Some(provider) = &self.provider else {
            return Vec::new();
        };
        let mut names = Vec::new();
        if provider.brave.is_some() {
            names.push("brave");
        }
        if provider.tavily.is_some() {
            names.push("tavily");
        }
        names
    }
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

impl ProviderConfig {
    /// 项目配置中已声明的协议、非空连接参数与同名模型覆盖全局配置。
    fn merge(&mut self, project: Self) {
        self.provider_type = project.provider_type.or(self.provider_type);
        if !project.base_url.is_empty() {
            self.base_url = project.base_url;
        }
        if !project.api_key.is_empty() {
            self.api_key = project.api_key;
        }
        self.models.extend(project.models);
    }
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

    /// 是否使用原生 web search 服务端工具（`annotation.web_search: false` 时关闭，
    /// 会话 web 开关开启且配置了外置 provider 时以客户端工具补全）
    pub fn declare_web_search(&self) -> bool {
        self.annotation
            .as_ref()
            .and_then(|a| a.web_search)
            .unwrap_or(true)
    }

    /// 该模型显式声明的文件编辑协议；未声明时为 `None`
    pub fn editor(&self) -> Option<crate::model::EditorKind> {
        self.annotation.as_ref().and_then(|a| a.editor)
    }

    /// 该模型的请求服务档位；`annotation.fast: true` 时以 `service_tier: "fast"` 发起请求
    pub fn service_tier(&self) -> Option<&'static str> {
        match self.annotation.as_ref().and_then(|a| a.fast) {
            Some(true) => Some("fast"),
            _ => None,
        }
    }

    /// 模型亲和组：显式声明优先，缺省沿用 provider 节点 id
    pub fn affinity(&self, provider_default: &str) -> String {
        self.annotation
            .as_ref()
            .and_then(|a| a.affinity.clone())
            .unwrap_or_else(|| provider_default.to_string())
    }
}

/// 模型级注记：细粒度关闭个别原生服务端工具的声明，以及文件编辑协议。
/// 用于未接线特定 server tool 的中转端点——带未识别条目时端点会整组丢弃工具声明，
/// 例如 `annotation: { "web_fetch": false }` 仅声明 web_search 以保住原生搜索
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAnnotation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<bool>,
    /// 原生 web search：`false` 标记模型/端点不支持原生搜索，
    /// 会话 web 开关开启且配置 `settings.web.search` 时改用外置客户端工具
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
    /// 模型亲和组：同组模型可互相复用原生回传状态（response_id 链、encrypted
    /// thinking、签名块），跨组切换时历史降维为通用重建。缺省为 provider 节点 id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<String>,
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
