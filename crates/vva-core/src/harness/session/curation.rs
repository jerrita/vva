//! 当前会话的沉淀标记工具，写入通过会话命令通道串行执行。

use super::{Command, SharedCommand};
use crate::model::ToolInput;
use crate::tools::{Tool, parse_args};
use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

pub(super) struct SetSessionCuration {
    commands: mpsc::WeakSender<Command>,
}

impl SetSessionCuration {
    pub(super) fn new(commands: mpsc::WeakSender<Command>) -> Self {
        Self { commands }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    enabled: bool,
}

#[async_trait]
impl Tool for SetSessionCuration {
    fn name(&self) -> &str {
        "SetSessionCuration"
    }

    fn description(&self) -> &str {
        "Mark this conversation for long-term memory curation before cleanup. Proactively set \
         enabled=true when it contains stable user preferences, confirmed decisions or constraints, \
         or reusable lessons. Temporary progress, one-off queries and routine task execution do \
         not need marking. Once marked, later conversation is covered too; repeated marking is \
         unnecessary. Set enabled=false to withdraw the request."
    }

    fn input(&self) -> ToolInput {
        ToolInput::Json(serde_json::json!({
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean"}
            },
            "required": ["enabled"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: Arguments = parse_args(arguments, self.name())?;
        let commands = self.commands.upgrade().ok_or("Session is closed")?;
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::Shared(SharedCommand::SetCuration {
                enabled: args.enabled,
                reply,
            }))
            .await
            .map_err(|_| "Session is closed")?;
        result.await.map_err(|_| "Session is closed")??;
        Ok(if args.enabled {
            "已标记当前会话，清理前将整理有长期价值的内容。"
        } else {
            "已撤销当前会话的沉淀标记。"
        }
        .into())
    }
}
