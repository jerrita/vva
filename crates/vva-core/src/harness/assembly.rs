use std::sync::Arc;

use async_trait::async_trait;

use crate::model::{Message, Session, ToolCall, ToolResult, Turn};
use crate::provider::{CompletionOptions, InputCapabilities, Provider};
use crate::tools::ToolRegistry;

pub struct AgentAssembly {
    pub session: Session,
    pub setup: Arc<dyn SessionSetup>,
}

#[derive(Clone)]
pub struct ExecutionPlan {
    pub provider: Arc<dyn Provider>,
    pub options: CompletionOptions,
    pub inputs: InputCapabilities,
    pub tools: ToolRegistry,
    pub notices: Vec<String>,
    pub external_web: Vec<String>,
}

pub struct PreparedInput {
    /// 仅在新会话首次落盘时写入；冷加载保留已有历史。
    pub initial: Vec<Turn>,
    pub before: Vec<Message>,
    pub message: Message,
    pub after: Vec<Message>,
    pub notices: Vec<String>,
}

impl PreparedInput {
    pub fn unchanged(message: Message) -> Self {
        Self {
            initial: Vec::new(),
            before: Vec::new(),
            message,
            after: Vec::new(),
            notices: Vec::new(),
        }
    }
}

pub struct ToolExecution {
    pub call: ToolCall,
    pub result: ToolResult,
}

/// 会话拥有的运行资源；宿主装配工具时复用这些句柄。
pub struct SessionServices {
    #[cfg(feature = "filesystem-tools")]
    pub hashline: Arc<crate::tools::hashline::HashlineSession>,
    pub ask_user_requests: tokio::sync::mpsc::Sender<crate::tools::ask_user::AskUserRequest>,
    pub background: Arc<crate::tools::background::BackgroundTasks>,
    pub shell_outputs: Arc<crate::tools::shell_output::ShellOutputs>,
}

#[async_trait]
pub trait SessionSetup: Send + Sync {
    async fn plan(
        &self,
        session: &Session,
        services: &SessionServices,
    ) -> Result<ExecutionPlan, String>;
    async fn prepare_input(
        &self,
        _session: &Session,
        input: Message,
        _new_session: bool,
    ) -> Result<PreparedInput, String> {
        Ok(PreparedInput::unchanged(input))
    }
    async fn after_tools(
        &self,
        _session: &Session,
        _batch: &[ToolExecution],
    ) -> Result<Vec<Message>, String> {
        Ok(Vec::new())
    }
    /// 配置命令由任务串行应用，宿主负责解析模型选择。
    fn configure(
        &self,
        _session: &mut Session,
        _model: Option<String>,
        _variant: Option<Option<String>>,
    ) -> Result<(), String> {
        Err("This session setup does not support model changes".into())
    }
    fn selection_saved(&self, _session: &Session) -> Result<(), String> {
        Ok(())
    }
    async fn title(&self, _session: &Session, _prompt: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    async fn abort_children(&self, _session_id: &str) {}
}

pub struct FixedSetup {
    plan: ExecutionPlan,
}

impl FixedSetup {
    pub fn new(plan: ExecutionPlan) -> Self {
        Self { plan }
    }
}

#[async_trait]
impl SessionSetup for FixedSetup {
    async fn plan(
        &self,
        _session: &Session,
        _services: &SessionServices,
    ) -> Result<ExecutionPlan, String> {
        Ok(self.plan.clone())
    }
}
