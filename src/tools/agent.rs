//! Agent 工具：派生 subagent 会话并等待其轮次结论。
//!
//! 子代理是真实持久化 session（独立 SessionTask 与 JSONL），前台阻塞等待结论、
//! 后台经 SessionBroker 回投父会话注入续跑；工具白名单由 agent 定义裁剪。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::Tool;
use crate::agents::AgentDef;
use crate::model::Session;
use crate::tools::filesystem::parse_args;

/// 派生上下文：父会话的定位与模型绑定信息
pub struct SubagentContext {
    pub parent_id: String,
    pub workspace_root: PathBuf,
    pub current_model: String,
    pub current_variant: Option<String>,
    pub web_search: bool,
    pub depth: u8,
}

impl SubagentContext {
    pub fn from_session(session: &Session) -> Self {
        Self {
            parent_id: session.id.clone(),
            workspace_root: session.workspace_root.clone(),
            current_model: session.current_model.clone(),
            current_variant: session.current_variant.clone(),
            web_search: session.web_search,
            depth: session.depth,
        }
    }
}

/// 子代理会话报告（回投通知与等待返回共用）
#[derive(Clone)]
pub struct SubagentReport {
    pub session_id: String,
    /// agent 定义名（顶层会话为空串）
    pub agent: String,
    pub title: String,
    /// 已完成轮次计数（每次 finish_turn 递增）
    pub turn_seq: u64,
    /// 本轮成功完成时的最终助手消息
    pub text: String,
    pub error: Option<String>,
}

/// spawn_subagent 返回：子会话 id 与提交前轮次基线（wait_turn 以此判定首轮结束）
#[derive(Debug)]
pub struct SubagentLaunch {
    pub session_id: String,
    pub turn_seq: u64,
}

/// tools 层的会话编排接口：由 harness 实现，提供子代理派生与跨会话投递。
/// 依赖方向与 AskUserRequest 一致：harness 依赖 tools 层类型。
#[async_trait]
pub trait SessionBroker: Send + Sync {
    /// 派生子代理会话并提交首轮 prompt（depth 超限报错）；
    /// background 时注册完成回投 watcher
    async fn spawn_subagent(
        &self,
        parent: &SubagentContext,
        def: &AgentDef,
        prompt: &str,
        description: &str,
        background: bool,
    ) -> Result<SubagentLaunch, String>;

    /// 向目标会话投递消息：空闲即触发轮次，忙时入队待轮次间隙处理
    async fn deliver(&self, session_id: &str, text: &str) -> Result<String, String>;

    /// 优先等待调用方尚未消费的投递回执；无回执时等待轮次计数超过 seen_seq
    async fn wait_turn(
        &self,
        session_id: &str,
        seen_seq: u64,
        timeout: Option<Duration>,
    ) -> Result<SubagentReport, String>;

    /// 读取目标会话报告（不等待）
    async fn session_report(&self, session_id: &str) -> Result<SubagentReport, String>;

    /// 中止目标会话当前轮次
    async fn abort_session(&self, session_id: &str) -> Result<(), String>;

    /// 中止指定会话派生的仍在运行的前台子代理（父会话被中止时联动）
    async fn abort_children(&self, parent_id: &str);
}

#[derive(Deserialize)]
struct AgentArgs {
    agent: String,
    description: String,
    prompt: String,
    #[serde(default)]
    run_in_background: bool,
}

pub struct AgentTool {
    broker: Arc<dyn SessionBroker>,
    defs: Arc<Vec<AgentDef>>,
    context: SubagentContext,
}

impl AgentTool {
    pub fn new(
        broker: Arc<dyn SessionBroker>,
        defs: Arc<Vec<AgentDef>>,
        context: SubagentContext,
    ) -> Self {
        Self {
            broker,
            defs,
            context,
        }
    }

    fn agent_enum(&self) -> Vec<String> {
        self.defs.iter().map(|def| def.name.clone()).collect()
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        "Agent"
    }

    fn description(&self) -> &str {
        "Launch a subagent that runs in its own session and returns its final message as the \
         result. Subagents work in the same workspace; they do not see this session's history, \
         so the prompt must be self-contained (goal, constraints, relevant paths). Multiple \
         Agent calls in one turn run concurrently. Set run_in_background to detach the \
         subagent: it keeps running, its final message is injected here as a system reminder \
         when it finishes, and you continue working in the meantime."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "enum": self.agent_enum(),
                    "description": "Agent type (toolset and system prompt) to launch."
                },
                "description": {
                    "type": "string",
                    "description": "3-5 word summary of what this subagent will do."
                },
                "prompt": {
                    "type": "string",
                    "description": "Complete, self-contained task for the subagent."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Detach the subagent instead of blocking for its result."
                }
            },
            "required": ["agent", "description", "prompt"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        self.run(arguments).await.map(|(text, _)| text)
    }

    async fn execute_result(&self, call: &crate::model::ToolCall) -> crate::model::ToolResult {
        match self.run(&call.arguments).await {
            Ok((text, task_id)) => {
                let mut result = crate::model::ToolResult::success(&call.id, text);
                result.background_task_id = task_id;
                result
            }
            Err(error) => crate::model::ToolResult::error(&call.id, error),
        }
    }
}

impl AgentTool {
    async fn run(&self, arguments: &str) -> Result<(String, Option<String>), String> {
        let args: AgentArgs = parse_args(arguments, "Agent")?;
        let def = self
            .defs
            .iter()
            .find(|def| def.name == args.agent)
            .ok_or_else(|| {
                format!(
                    "Unknown agent '{}'; available: {}",
                    args.agent,
                    self.agent_enum().join(", ")
                )
            })?;
        let launch = self
            .broker
            .spawn_subagent(
                &self.context,
                def,
                &args.prompt,
                &args.description,
                args.run_in_background,
            )
            .await?;
        if args.run_in_background {
            return Ok((
                format!(
                    "Background agent started: {} (type: {})\ndescription: {}\nIt runs in its \
                 own session; its final message will be injected here as a system reminder when \
                 it finishes.",
                    launch.session_id, def.name, args.description
                ),
                Some(launch.session_id),
            ));
        }
        let report = self
            .broker
            .wait_turn(&launch.session_id, launch.turn_seq, None)
            .await?;
        if let Some(error) = &report.error {
            return Err(format!("Subagent {} failed: {error}", report.session_id));
        }
        if report.text.trim().is_empty() {
            return Err(format!(
                "Subagent {} ({}) ended without a final message (aborted or failed before \
                 responding); open the session to inspect its transcript",
                report.session_id, def.name
            ));
        }
        Ok((report.text, None))
    }
}

/// 完成注入轮次内联的最终消息字节上限（全文在其会话 JSONL 中）
const SUBAGENT_REMINDER_TEXT_BYTES: usize = 8 * 1024;

/// 后台子代理完成通知轮次正文：状态行 + 截尾最终消息，background-task reminder 包裹
pub fn format_subagent_reminder(report: &SubagentReport) -> String {
    let mut notification = format!(
        "Background agent {} ({}) finished.",
        report.session_id, report.agent
    );
    if !report.title.is_empty() {
        notification.push_str(&format!("\ntitle: {}", report.title));
    }
    let text = truncate_head_bytes(&report.text, SUBAGENT_REMINDER_TEXT_BYTES);
    if let Some(error) = &report.error {
        notification.push_str(&format!("\nTurn ended: {error}"));
    } else if text.trim().is_empty() {
        notification
            .push_str("\nIt produced no final message (aborted or failed before responding).");
    } else {
        notification.push_str(&format!(
            "\n--- final message (first {SUBAGENT_REMINDER_TEXT_BYTES} bytes) ---\n{text}"
        ));
    }
    notification.push_str(&format!(
        "\nUse SendMessage / WaitSession with session_id \"{}\" to continue its conversation.",
        report.session_id
    ));
    crate::system::format_background_task_reminder(&notification)
}

/// 头部截断到字节上限且不切断 UTF-8 字符边界
fn truncate_head_bytes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    /// 派生必败的 broker 桩：参数校验路径不触发真实派生
    struct ErrBroker;

    #[async_trait]
    impl SessionBroker for ErrBroker {
        async fn spawn_subagent(
            &self,
            _parent: &SubagentContext,
            _def: &AgentDef,
            _prompt: &str,
            _description: &str,
            _background: bool,
        ) -> Result<SubagentLaunch, String> {
            Err("stub broker".into())
        }
        async fn deliver(&self, _session_id: &str, _text: &str) -> Result<String, String> {
            Err("stub broker".into())
        }
        async fn wait_turn(
            &self,
            _session_id: &str,
            _seen_seq: u64,
            _timeout: Option<Duration>,
        ) -> Result<SubagentReport, String> {
            Err("stub broker".into())
        }
        async fn session_report(&self, _session_id: &str) -> Result<SubagentReport, String> {
            Err("stub broker".into())
        }
        async fn abort_session(&self, _session_id: &str) -> Result<(), String> {
            Err("stub broker".into())
        }
        async fn abort_children(&self, _parent_id: &str) {}
    }

    fn tool() -> AgentTool {
        AgentTool::new(
            Arc::new(ErrBroker),
            Arc::new(crate::agents::builtin_agents()),
            SubagentContext {
                parent_id: "sess_p0".into(),
                workspace_root: PathBuf::from("."),
                current_model: "p/m".into(),
                current_variant: None,
                web_search: false,
                depth: 0,
            },
        )
    }

    #[tokio::test]
    async fn rejects_unknown_agent_type() {
        let error = tool()
            .execute(r#"{"agent":"nope","description":"d","prompt":"p"}"#)
            .await
            .unwrap_err();
        assert!(error.contains("Unknown agent 'nope'"), "{error}");
        assert!(error.contains("explore"), "{error}");
    }

    #[tokio::test]
    async fn rejects_missing_arguments() {
        // 缺 description 与 prompt：注册表参数校验层拦截
        let error = tool().execute(r#"{"agent":"general"}"#).await.unwrap_err();
        assert!(error.contains("description"), "{error}");
    }

    fn report(text: &str) -> SubagentReport {
        SubagentReport {
            session_id: "sess_a1".into(),
            agent: "general".into(),
            title: "[general] probe".into(),
            turn_seq: 1,
            error: None,
            text: text.into(),
        }
    }

    #[test]
    fn reminder_wraps_status_and_tail() {
        let reminder = format_subagent_reminder(&report("all done"));
        assert!(reminder.starts_with("<system-reminder type=\"background-task\">"));
        assert!(reminder.contains("Background agent sess_a1 (general) finished."));
        assert!(reminder.contains("title: [general] probe"));
        assert!(reminder.contains("--- final message"));
        assert!(reminder.contains("SendMessage / WaitSession"));
    }

    #[test]
    fn reminder_marks_missing_final_message() {
        let reminder = format_subagent_reminder(&report("  "));
        assert!(reminder.contains("produced no final message"));
        assert!(!reminder.contains("--- final message"));
    }

    #[test]
    fn reminder_truncates_long_text_at_char_boundary() {
        let text: String = "好".repeat(5000);
        let reminder = format_subagent_reminder(&report(&text));
        let truncated: String = reminder.chars().filter(|ch| *ch == '好').collect();
        // 8 KiB 上限下最多保留 4096 个三字节字符
        assert!(truncated.chars().count() <= 4096);
    }

    #[test]
    fn head_truncation_keeps_valid_utf8() {
        let truncated = truncate_head_bytes("好".repeat(100).as_str(), 10);
        assert!(truncated.chars().all(|ch| ch == '好'));
        assert_eq!(truncated.chars().count(), 3);
    }
}
