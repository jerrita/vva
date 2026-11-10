//! 当前会话的沉淀标记工具，独立应用记录中的写入串行执行。

use crate::model::ToolInput;
use crate::tools::{Tool, parse_args};
use async_trait::async_trait;
use serde::Deserialize;

pub(crate) struct SetSessionCuration {
    store: std::sync::Arc<crate::app::store::AppSessionStore>,
    session_id: String,
}

impl SetSessionCuration {
    pub(crate) fn new(
        store: std::sync::Arc<crate::app::store::AppSessionStore>,
        session_id: String,
    ) -> Self {
        Self { store, session_id }
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
        self.store
            .update_record(&self.session_id, |record| match &mut record.application {
                crate::app::records::Application::Space(space) => {
                    let changed = space.curation_requested != args.enabled;
                    space.curation_requested = args.enabled;
                    Ok(changed)
                }
                crate::app::records::Application::Coding => Err("当前会话不支持沉淀标记".into()),
            })?;
        Ok(if args.enabled {
            "已标记当前会话，清理前将整理有长期价值的内容。"
        } else {
            "已撤销当前会话的沉淀标记。"
        }
        .into())
    }
}
