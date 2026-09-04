use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::resolve_provider;
use crate::model::{ContentPart, Message, Session, Turn};
use crate::provider::{CompletionOptions, Provider, ProviderError};
use crate::storage::{SessionStore, StoreError};
use crate::tools::ToolRegistry;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_iterations: usize,
    pub completion_options: CompletionOptions,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 15,
            completion_options: CompletionOptions::default(),
        }
    }
}

pub use crate::event::AgentEvent;

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

/// 组装一轮 Agent 执行引擎：Provider 解析 → 工具集 → CompletionOptions → AgentEngine。
/// CLI 与 TUI 共用的唯一组装路径，同时返回能力协商降级通知。
pub fn build_engine(
    app_config: &Option<crate::config::AppConfig>,
    model_spec: &str,
    variant: Option<&str>,
    web_search: bool,
    workspace: &Path,
    store: Arc<dyn SessionStore>,
) -> Result<(AgentEngine, Vec<String>), String> {
    let (provider, effective_model, reasoning_effort) =
        resolve_provider(app_config, model_spec, variant)?;

    let completion_options = CompletionOptions {
        model: effective_model,
        enable_web_search: web_search,
        reasoning_effort,
        ..Default::default()
    };
    let notices = capability_notices(provider.as_ref(), &completion_options);

    let tools = ToolRegistry::with_workspace_defaults(workspace);
    let engine = AgentEngine::new(
        provider,
        store,
        tools,
        AgentConfig {
            completion_options,
            ..Default::default()
        },
    );
    Ok((engine, notices))
}

pub struct AgentEngine {
    provider: Arc<dyn Provider>,
    store: Arc<dyn SessionStore>,
    tools: ToolRegistry,
    config: AgentConfig,
}

impl AgentEngine {
    pub fn new(
        provider: Arc<dyn Provider>,
        store: Arc<dyn SessionStore>,
        tools: ToolRegistry,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            store,
            tools,
            config,
        }
    }

    /// 供应商名称（诊断展示用）
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// 生效的 API 模型名（诊断展示用）
    pub fn model(&self) -> &str {
        &self.config.completion_options.model
    }

    /// 执行单轮对话或多轮自动工具调度循环
    pub async fn run_turn(
        &self,
        session: &mut Session,
        event_sender: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<(), AgentError> {
        let mut iteration = 0;

        while iteration < self.config.max_iterations {
            iteration += 1;
            info!(
                iteration,
                provider = self.provider.name(),
                model = %self.config.completion_options.model,
                "Calling provider complete_stream"
            );

            let tool_defs = self.tools.definitions();
            let parent_turn_id = session.last_turn_id();

            let response = self
                .provider
                .complete_stream(
                    session,
                    &tool_defs,
                    &self.config.completion_options,
                    event_sender.clone(),
                )
                .await?;

            // 检查是否有待执行的工具调用
            let mut tool_calls = Vec::new();
            for msg in &response.messages {
                for part in &msg.parts {
                    if let ContentPart::ToolCall(call) = part {
                        tool_calls.push(call.clone());
                    }
                }
            }

            // 将 Assistant 轮次组装并持久化
            let mut asst_turn = Turn::assistant_turn(response.messages, parent_turn_id);
            asst_turn.usage = response.usage;
            asst_turn.meta = response.meta;

            self.store.append_turn(&session.id, &asst_turn).await?;
            let asst_turn_id = asst_turn.id.clone();
            session.append_turn(asst_turn);

            // 如果没有工具调用，本轮结束
            if tool_calls.is_empty() {
                if let Some(tx) = &event_sender {
                    let _ = tx.send(AgentEvent::Finished).await;
                }
                return Ok(());
            }

            // 执行所有工具调用并产生 ToolResult 消息
            let mut tool_messages = Vec::new();
            for call in tool_calls {
                if let Some(tx) = &event_sender {
                    let _ = tx
                        .send(AgentEvent::ToolExecuting {
                            name: call.name.clone(),
                            args: call.arguments.clone(),
                        })
                        .await;
                }

                let result = self.tools.execute_call(&call).await;

                if let Some(tx) = &event_sender {
                    let _ = tx
                        .send(AgentEvent::ToolFinished {
                            name: call.name.clone(),
                            result: result.content.clone(),
                            is_error: result.is_error,
                        })
                        .await;
                }

                tool_messages.push(Message::tool_result(result));
            }

            // 将 Tool 响应存为新 Turn 并持久化
            let tool_turn = Turn::new(crate::model::Role::Tool, tool_messages, Some(asst_turn_id));
            self.store.append_turn(&session.id, &tool_turn).await?;
            session.append_turn(tool_turn);
        }

        warn!(
            max_iterations = self.config.max_iterations,
            "Agent loop reached max iterations"
        );
        if let Some(tx) = &event_sender {
            let _ = tx.send(AgentEvent::Finished).await;
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("Storage error: {0}")]
    Storage(#[from] StoreError),
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
