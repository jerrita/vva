use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use vva_core::harness::*;
use vva_core::model::{Message, Session};
use vva_core::provider::{CompletionOptions, ProviderResponse};
use vva_core::tools::agent::SubagentReport;

// ---- 共享夹具 ----

pub(crate) fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vva-{tag}-{}", uuid::Uuid::new_v4()))
}

pub(crate) struct ScriptedProvider {
    pub(crate) script: fn(bool) -> Vec<Message>,
    pub(crate) called: std::sync::atomic::AtomicBool,
}

impl ScriptedProvider {
    pub(crate) fn new(script: fn(bool) -> Vec<Message>) -> Arc<Self> {
        Arc::new(Self {
            script,
            called: Default::default(),
        })
    }
}

#[async_trait::async_trait]
impl vva_core::provider::Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted-test"
    }
    fn capabilities(&self) -> vva_core::provider::ProviderCapabilities {
        Default::default()
    }
    async fn complete(
        &self,
        _: &Session,
        _: &[vva_core::model::ToolDefinition],
        _: &CompletionOptions,
    ) -> Result<ProviderResponse, vva_core::provider::ProviderError> {
        let repeat = self.called.swap(true, std::sync::atomic::Ordering::SeqCst);
        Ok(ProviderResponse {
            messages: (self.script)(repeat),
            usage: None,
            meta: Default::default(),
        })
    }
    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[vva_core::model::ToolDefinition],
        options: &CompletionOptions,
        _: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, vva_core::provider::ProviderError> {
        self.complete(session, tools, options).await
    }
}

pub(crate) fn engine(
    provider: Arc<ScriptedProvider>,
    tools: vva_core::tools::ToolRegistry,
) -> ExecutionPlan {
    ExecutionPlan {
        provider,
        tools,
        options: CompletionOptions::default(),
        notices: Vec::new(),
        external_web: Vec::new(),
        inputs: Default::default(),
    }
}

pub(crate) fn done_provider() -> Arc<ScriptedProvider> {
    ScriptedProvider::new(|_| vec![Message::text("done")])
}

/// 测试工具：可选阻塞语义，参数原样回显
pub(crate) struct TestTool {
    pub(crate) name: &'static str,
    /// 等待栅栏 (并发验证) 或永不完成 (中止验证)
    pub(crate) blocking: TestBlock,
}

pub(crate) enum TestBlock {
    Barrier(tokio::sync::Barrier),
    Forever,
    Gate {
        permits: Arc<tokio::sync::Semaphore>,
        finished: Arc<tokio::sync::Semaphore>,
    },
}

#[async_trait::async_trait]
impl vva_core::tools::Tool for TestTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn input(&self) -> vva_core::model::ToolInput {
        vva_core::model::ToolInput::Json({
            serde_json::json!({"type": "object", "properties": {"label": {"type": "string"}}})
        })
    }
    async fn execute(&self, arguments: &str) -> Result<String, String> {
        match &self.blocking {
            TestBlock::Barrier(barrier) => {
                barrier.wait().await;
                Ok(arguments.into())
            }
            TestBlock::Forever => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(String::new())
            }
            TestBlock::Gate { permits, finished } => {
                permits.acquire().await.unwrap().forget();
                finished.add_permits(1);
                Ok(arguments.into())
            }
        }
    }
}

pub(crate) fn registry_with(tool: TestTool) -> vva_core::tools::ToolRegistry {
    let mut tools = vva_core::tools::ToolRegistry::new();
    tools.register(Arc::new(tool));
    tools
}

/// 等待后台任务达到期望终态（真实子进程，轮询 snapshot）
pub(crate) fn async_report(text: &str) -> SubagentReport {
    SubagentReport {
        session_id: "sess_worker".into(),
        agent: "general".into(),
        title: String::new(),
        turn_seq: 1,
        text: text.into(),
        error: None,
    }
}

pub(crate) fn fixed_setup() -> Arc<dyn SessionSetup> {
    Arc::new(FixedSetup::new(engine(done_provider(), Default::default())))
}
