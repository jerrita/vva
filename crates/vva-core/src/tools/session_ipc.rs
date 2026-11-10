//! 跨会话消息工具：session 间类 actor 通信。
//!
//! SendMessage 投递消息（空闲即触发轮次，忙时入队），回复经运行时自动回投。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::Tool;
use super::agent::SessionBroker;
use crate::tools::parse_args;

#[derive(Deserialize)]
struct SendMessageArgs {
    session_id: String,
    text: String,
}

pub struct SendMessageTool {
    broker: Arc<dyn SessionBroker>,
}

impl SendMessageTool {
    pub fn new(broker: Arc<dyn SessionBroker>) -> Self {
        Self { broker }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "SendMessage"
    }

    fn description(&self) -> &str {
        "Deliver a message to another session by its sess_ id (e.g. a subagent you launched). \
         An idle session starts a new turn; a running session queues the message until its \
         current turn ends. The reply automatically resumes this session, including after you \
         end your response. Continue other work, or end your response when nothing else can proceed; \
         do not sleep or poll for the reply."
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Target session id (starts with sess_)."
                    },
                    "text": {
                        "type": "string",
                        "description": "Message body to deliver."
                    }
                },
                "required": ["session_id", "text"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: SendMessageArgs = parse_args(arguments, "SendMessage")?;
        if args.text.trim().is_empty() {
            return Err("SendMessage text must not be empty".into());
        }
        self.broker.deliver(&args.session_id, &args.text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use crate::tools::agent::{SubagentContext, SubagentLaunch, SubagentReport};

    /// 记录投递调用并返回固定报告的 broker 桩
    struct RecordingBroker {
        delivered: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl SessionBroker for RecordingBroker {
        async fn spawn_subagent(
            &self,
            _parent: &SubagentContext,
            _def: &crate::agents::AgentDef,
            _prompt: &str,
            _description: &str,
            _background: bool,
        ) -> Result<SubagentLaunch, String> {
            Err("stub broker".into())
        }
        async fn deliver(&self, session_id: &str, text: &str) -> Result<String, String> {
            self.delivered
                .lock()
                .unwrap()
                .push((session_id.into(), text.into()));
            Ok("Delivered".into())
        }
        async fn wait_turn(
            &self,
            _session_id: &str,
            _seen_seq: u64,
            _timeout: Option<std::time::Duration>,
        ) -> Result<SubagentReport, String> {
            Ok(self.report_text())
        }
        async fn session_report(&self, _session_id: &str) -> Result<SubagentReport, String> {
            Ok(self.report_text())
        }
        async fn abort_session(&self, _session_id: &str) -> Result<(), String> {
            Err("stub broker".into())
        }
        async fn abort_children(&self, _parent_id: &str) {}
    }

    impl RecordingBroker {
        fn report_text(&self) -> SubagentReport {
            SubagentReport {
                session_id: "sess_b2".into(),
                agent: "general".into(),
                title: String::new(),
                turn_seq: 2,
                error: None,
                text: "reply body".into(),
            }
        }
    }

    fn broker() -> Arc<RecordingBroker> {
        Arc::new(RecordingBroker {
            delivered: std::sync::Mutex::new(Vec::new()),
        })
    }

    #[tokio::test]
    async fn send_message_passes_through_and_rejects_empty_text() {
        let broker = broker();
        let tool = SendMessageTool::new(broker.clone());
        let ack = tool
            .execute(r#"{"session_id":"sess_b2","text":"hello"}"#)
            .await
            .unwrap();
        assert_eq!(ack, "Delivered");
        assert_eq!(
            broker.delivered.lock().unwrap().as_slice(),
            [("sess_b2".to_string(), "hello".to_string())]
        );

        let error = tool
            .execute(r#"{"session_id":"sess_b2","text":"  "}"#)
            .await
            .unwrap_err();
        assert!(error.contains("must not be empty"), "{error}");
    }
}
