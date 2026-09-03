use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

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
            max_iterations: 10,
            completion_options: CompletionOptions::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Thought(String),
    ToolExecuting { name: String, args: String },
    ToolFinished { name: String, result: String, is_error: bool },
    Text(String),
    Finished,
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
                "Calling provider complete"
            );

            let tool_defs = self.tools.definitions();
            let parent_turn_id = session.last_turn_id();

            let response = self
                .provider
                .complete(session, &tool_defs, &self.config.completion_options)
                .await?;

            // 派发事件
            for msg in &response.messages {
                for part in &msg.parts {
                    match part {
                        ContentPart::Reasoning { content, .. } => {
                            if let Some(tx) = &event_sender {
                                let _ = tx.send(AgentEvent::Thought(content.clone())).await;
                            }
                        }
                        ContentPart::Text { text, .. } => {
                            if let Some(tx) = &event_sender {
                                let _ = tx.send(AgentEvent::Text(text.clone())).await;
                            }
                        }
                        _ => {}
                    }
                }
            }

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
                    let _ = tx.send(AgentEvent::ToolExecuting {
                        name: call.name.clone(),
                        args: call.arguments.clone(),
                    }).await;
                }

                let result = self.tools.execute_call(&call).await;

                if let Some(tx) = &event_sender {
                    let _ = tx.send(AgentEvent::ToolFinished {
                        name: call.name.clone(),
                        result: result.content.clone(),
                        is_error: result.is_error,
                    }).await;
                }

                tool_messages.push(Message::tool_result(result));
            }

            // 将 Tool 响应存为新 Turn 并持久化
            let tool_turn = Turn::new(crate::model::Role::Tool, tool_messages, Some(asst_turn_id));
            self.store.append_turn(&session.id, &tool_turn).await?;
            session.append_turn(tool_turn);
        }

        warn!(max_iterations = self.config.max_iterations, "Agent loop reached max iterations");
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

