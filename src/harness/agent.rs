use std::path::PathBuf;
use std::sync::Arc;

use crate::config::AppConfig;
use crate::model::{Message, Session, Turn};
use crate::provider::{CompletionOptions, Provider};
use crate::tools::ToolRegistry;

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
}

impl AgentEngine {
    pub fn new(
        config: &AppConfig,
        session: &Session,
        tool_outputs: PathBuf,
    ) -> Result<Self, String> {
        let resolved = config
            .resolve_model(&session.current_model, session.current_variant.as_deref())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            provider: resolved.create_provider(),
            tools: ToolRegistry::with_workspace_defaults(
                &session.workspace_root,
                Some(tool_outputs),
            ),
            options: CompletionOptions {
                model: resolved.model_id,
                reasoning_effort: resolved.selected_variant,
                enable_web_search: session.web_search,
                ..Default::default()
            },
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
        "Generate a brief title (3-6 words, plain text, no quotes) summarizing this prompt. Return only the title.\n\n{prompt}"
    ))));
    let options = CompletionOptions {
        model: resolved.model_id.clone(),
        temperature: Some(0.3),
        max_tokens: Some(30),
        enable_web_search: false,
        reasoning_effort: None,
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

    #[test]
    fn test_capability_notices_flag_unsupported_options() {
        let provider = crate::provider::anthropic::AnthropicProvider::new("k");
        let options = CompletionOptions {
            model: "claude-x".into(),
            enable_web_search: true,
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let notices = capability_notices(&provider, &options);
        assert_eq!(notices.len(), 2);
        assert!(notices[0].contains("native search"));
        assert!(notices[1].contains("Reasoning effort 'high'"));

        // 全能力供应商（Responses）无通知
        let response = crate::provider::response::ResponseProvider::new("k");
        assert!(capability_notices(&response, &options).is_empty());
    }
}
