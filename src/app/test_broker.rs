//! 测试共享桩：无编排能力的 broker。

use crate::tools::agent::{SessionBroker, SubagentContext, SubagentLaunch, SubagentReport};

/// 拒绝派生与投递的 broker 桩；携带报告时 `session_report` 返回该报告
pub(crate) struct StubBroker(pub Option<SubagentReport>);

const STUB: &str = "stub broker";

#[async_trait::async_trait]
impl SessionBroker for StubBroker {
    async fn spawn_subagent(
        &self,
        _parent: &SubagentContext,
        _def: &crate::agents::AgentDef,
        _prompt: &str,
        _description: &str,
        _background: bool,
    ) -> Result<SubagentLaunch, String> {
        Err(STUB.into())
    }
    async fn deliver(&self, _session_id: &str, _text: &str) -> Result<String, String> {
        Err(STUB.into())
    }
    async fn wait_turn(
        &self,
        _session_id: &str,
        _seen_seq: u64,
        _timeout: Option<std::time::Duration>,
    ) -> Result<SubagentReport, String> {
        Err(STUB.into())
    }
    async fn session_report(&self, _session_id: &str) -> Result<SubagentReport, String> {
        self.0.clone().ok_or_else(|| STUB.into())
    }
    async fn abort_session(&self, _session_id: &str) -> Result<(), String> {
        Err(STUB.into())
    }
    async fn abort_children(&self, _parent_id: &str) {}
}
