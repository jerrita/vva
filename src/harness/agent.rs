use std::path::PathBuf;
use std::sync::Arc;

use crate::config::AppConfig;
use crate::model::{Message, Session, Turn};
use crate::provider::{CompletionOptions, Provider};
use crate::tools::ToolRegistry;
use tokio::sync::mpsc;

/// 能力协商：补全选项与供应商原生能力不匹配时生成显式降级通知（不静默失效）
pub fn capability_notices(provider: &dyn Provider, options: &CompletionOptions) -> Vec<String> {
    let caps = provider.capabilities();
    let mut notices = Vec::new();
    if options.enable_web_search && !caps.supports_native_search {
        notices.push(format!(
            "Web search is enabled but provider '{}' has no native search support; the option is ignored.",
            provider.name()
        ));
    }
    if let Some(effort) = &options.reasoning_effort
        && !effort.is_empty()
        && !caps.supports_reasoning_effort
    {
        notices.push(format!(
            "Reasoning effort '{effort}' is set but provider '{}' does not support the effort parameter; it is ignored.",
            provider.name()
        ));
    }
    notices
}

pub(super) struct AgentEngine {
    pub provider: Arc<dyn Provider>,
    pub tools: ToolRegistry,
    pub options: CompletionOptions,
    /// 当前模型的输入能力，供提交前校验用户附件
    pub(super) inputs: crate::config::resolver::InputCapabilities,
}

impl AgentEngine {
    pub fn new(
        config: &AppConfig,
        session: &Session,
        tool_outputs: PathBuf,
        hashline: Arc<crate::tools::hashline::HashlineSession>,
        ask_user_requests: mpsc::Sender<crate::tools::ask_user::AskUserRequest>,
        skills: Vec<crate::skills::Skill>,
    ) -> Result<Self, String> {
        let resolved = config
            .resolve_model(&session.current_model, session.current_variant.as_deref())
            .map_err(|e| e.to_string())?;
        let inputs = resolved.input_capabilities();
        inputs.validate_session(session)?;
        let mut tools = ToolRegistry::with_editor_session(
            &session.workspace_root,
            Some(tool_outputs),
            session.editor,
            hashline,
        );
        let mut read = crate::tools::filesystem::ReadTool::new(&session.workspace_root)
            .with_inputs(inputs.clone())
            .with_editor(session.editor);
        if let Some(hashline) = tools.hashline_session() {
            read = read.with_hashline(hashline);
        }
        tools.register(Arc::new(read));
        tools.register(Arc::new(crate::tools::ask_user::AskUserTool::new(
            ask_user_requests,
        )));
        tools.register(Arc::new(crate::tools::skill::SkillTool::new(Arc::new(
            skills,
        ))));
        let provider = resolved.create_provider();
        let max_tokens = resolved
            .model_config
            .limit
            .as_ref()
            .map(|limit| {
                u32::try_from(limit.output)
                    .map_err(|_| format!("Model output limit exceeds u32: {}", limit.output))
            })
            .transpose()?;
        let options = CompletionOptions {
            model: resolved.model_id,
            max_tokens,
            reasoning_effort: resolved.selected_variant,
            enable_web_search: session.web_search,
            enable_web_fetch: resolved.model_config.declare_web_fetch(),
            service_tier: resolved.model_config.service_tier().map(str::to_string),
            ..Default::default()
        };
        // 开启原生联网搜索时预留服务端工具名：端点未代执行而由模型误发起
        // 客户端调用时，注册表给出显式诊断而非误导性的 Tool not found
        if options.enable_web_search {
            let mut server_tools = provider.capabilities().server_tool_names.clone();
            if !options.enable_web_fetch {
                server_tools.retain(|name| *name != "web_fetch");
            }
            tools.reserve_server_tools(server_tools);
        }
        Ok(Self {
            provider,
            tools,
            options,
            inputs,
        })
    }
}

pub(super) async fn generate_title(
    config: &AppConfig,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    let resolved = config
        .resolve_model(
            config.settings.small_model.as_deref().unwrap_or(model),
            None,
        )
        .map_err(|e| e.to_string())?;
    let mut session = Session::new(".", None);
    session.append_turn(Turn::user_turn(Message::text(format!(
        "Generate a brief title (3-6 words, plain text, no quotes) summarizing this prompt. Put spaces between Chinese and English text. Return only the title.\n\n{prompt}"
    ))));
    let options = CompletionOptions {
        model: resolved.model_id.clone(),
        temperature: Some(0.3),
        max_tokens: Some(30),
        enable_web_search: false,
        enable_web_fetch: false,
        reasoning_effort: None,
        service_tier: resolved.model_config.service_tier().map(str::to_string),
    };
    let response = resolved
        .create_provider()
        .complete(&session, &[], &options)
        .await
        .map_err(|e| e.to_string())?;
    Ok(response
        .messages
        .iter()
        .map(Message::text_content)
        .collect::<String>()
        .trim()
        .trim_matches(['"', '\'', '`', '#', '*'])
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderError, ProviderResponse};

    /// 无原生搜索能力的供应商桩：覆盖能力协商的降级通知分支
    struct NoSearchProvider;

    #[async_trait::async_trait]
    impl Provider for NoSearchProvider {
        fn name(&self) -> &str {
            "no-search"
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            crate::provider::ProviderCapabilities {
                supports_native_search: false,
                supports_reasoning_effort: false,
                server_tool_names: Vec::new(),
            }
        }

        async fn complete(
            &self,
            _session: &Session,
            _tools: &[crate::model::ToolDefinition],
            _options: &CompletionOptions,
        ) -> Result<ProviderResponse, ProviderError> {
            unimplemented!()
        }

        async fn complete_stream(
            &self,
            _session: &Session,
            _tools: &[crate::model::ToolDefinition],
            _options: &CompletionOptions,
            _event_sender: Option<tokio::sync::mpsc::Sender<crate::event::AgentEvent>>,
        ) -> Result<ProviderResponse, ProviderError> {
            unimplemented!()
        }
    }

    #[test]
    fn test_capability_notices_flag_unsupported_options() {
        // 全能力供应商（Responses）无通知
        let options = CompletionOptions {
            model: "claude-x".into(),
            enable_web_search: true,
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let response = crate::provider::response::ResponseProvider::new("k");
        assert!(capability_notices(&response, &options).is_empty());

        // 无原生搜索能力的供应商生成搜索降级通知
        let search_only = CompletionOptions {
            model: "claude-x".into(),
            enable_web_search: true,
            ..Default::default()
        };
        let notices = capability_notices(&NoSearchProvider, &search_only);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("native search"));
        assert!(notices[0].contains("no-search"));
    }

    #[test]
    fn test_capability_notices_flag_reasoning_effort_only() {
        // Anthropic 已支持原生搜索，仅推理档位生成降级通知
        let provider = crate::provider::anthropic::AnthropicProvider::new("k");
        let options = CompletionOptions {
            model: "claude-x".into(),
            enable_web_search: true,
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let notices = capability_notices(&provider, &options);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("Reasoning effort 'high'"));
    }
}
