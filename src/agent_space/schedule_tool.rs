//! Schedule 工具：让 agent 管理定时任务（create / list / pause / resume / delete）。
//!
//! 与 `SendFile` 同样按会话 origin 运行期注入。参数在此校验，错误当场返回给模型。
//! `run`（立即执行）需要 Runtime，只提供 `/schedule run` 命令。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::scheduler::{RawTask, ScheduleStore, Task, describe_task, format_time, now_minute};
use crate::tools::{Tool, parse_args};

struct ScheduleTool {
    store: Arc<ScheduleStore>,
    channel: String,
    /// 当前会话的聊天，形如 `channel:chat_id`
    binding: String,
    session_id: String,
}

#[derive(Deserialize)]
struct Arguments {
    action: String,
    id: Option<String>,
    cron: Option<String>,
    at: Option<String>,
    every: Option<String>,
    prompt: Option<String>,
    /// `session`：在当前会话内执行；缺省在新会话中执行
    target: Option<String>,
    bindings: Option<Vec<String>>,
}

impl ScheduleTool {
    fn create(&self, args: Arguments) -> Result<String, String> {
        let session = match args.target.as_deref() {
            None | Some("new") => None,
            Some("session") => Some(self.session_id.clone()),
            Some(other) => return Err(format!("target `{other}` 非法，应为 new 或 session")),
        };
        let bindings = match args.bindings {
            Some(list) => list,
            // 新会话执行时需要来源聊天；通知目标统一由 home bindings 决定。
            None if session.is_none() => vec![self.binding.clone()],
            None => Vec::new(),
        };
        let prefix = format!("{}:", self.channel);
        if let Some(bad) = bindings.iter().find(|b| !b.starts_with(&prefix)) {
            return Err(format!("binding `{bad}` 非法，当前仅支持 `{prefix}…`"));
        }
        let task = Task::try_from(RawTask {
            id: args
                .id
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..6].to_string()),
            cron: args.cron,
            at: args.at,
            every: args.every,
            prompt: args.prompt.ok_or("create 需要 prompt")?,
            session,
            bindings,
        })?;
        let state = self
            .store
            .add(task.clone())
            .map_err(|e| format!("{e}（当前时间 {}）", format_time(now_minute())))?;
        Ok(format!("已创建\n{}", describe_task(&task, &state)))
    }

    fn require_id(args: &Arguments) -> Result<&str, String> {
        args.id.as_deref().ok_or_else(|| "需要 id".into())
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "Schedule"
    }

    fn description(&self) -> &str {
        "Manage scheduled tasks that run on a timer and notify chats. Use only when the user \
         asks to create, view, pause, resume or delete a scheduled task or reminder. \
         Exactly one trigger: `cron` (5 fields: min hour day month weekday, local time), \
         `at` (one-shot local time, e.g. 2026-10-11T09:00) or `every` (30m / 2h / 1d, counted \
         from the end of the previous run). By default each run happens in a fresh session \
         with no history, so `prompt` must be fully self-contained (what to do, what to check, \
         what to report) and should say when to stay silent (the reply is then exactly \
         [SILENT]). Use target=session only for short-lived follow-ups in this very \
         conversation: sessions are cleaned up after being idle, which ends the task. \
         `bindings[0]` selects the source chat for a fresh execution session and defaults \
         to the current chat. All results and background notifications go only to home \
         chats configured with /sethome; remaining bindings do not select recipients."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["create", "list", "pause", "resume", "delete"]},
                "id": {"type": "string", "description": "Task id (prefix allowed) for pause/resume/delete; optional custom id for create"},
                "cron": {"type": "string", "description": "5-field cron expression"},
                "at": {"type": "string", "description": "One-shot local time, YYYY-MM-DDTHH:MM"},
                "every": {"type": "string", "description": "Interval such as 30m, 2h, 1d"},
                "prompt": {"type": "string", "description": "Self-contained instruction executed at each trigger"},
                "target": {"type": "string", "enum": ["new", "session"], "description": "Where to run: a fresh session (default) or this conversation"},
                "bindings": {"type": "array", "items": {"type": "string"}, "description": "First binding selects the source chat for a fresh session, e.g. [\"qq:c2c:<openid>\"]; defaults to the current chat. Notifications go only to configured home chats."}
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: Arguments = parse_args(arguments, "Schedule")?;
        match args.action.as_str() {
            "create" => self.create(args),
            "list" => {
                let tasks = self.store.list();
                let now = format_time(now_minute());
                if tasks.is_empty() {
                    return Ok(format!("暂无定时任务（当前时间 {now}）"));
                }
                let lines: Vec<_> = tasks.iter().map(|(t, s)| describe_task(t, s)).collect();
                Ok(format!("当前时间 {now}\n{}", lines.join("\n")))
            }
            "pause" => {
                let task = self.store.set_enabled(Self::require_id(&args)?, false)?;
                Ok(format!("已暂停 {}", task.id))
            }
            "resume" => {
                let task = self.store.set_enabled(Self::require_id(&args)?, true)?;
                Ok(format!("已恢复 {}", task.id))
            }
            "delete" => {
                let task = self.store.remove(Self::require_id(&args)?)?;
                Ok(format!("已删除 {}", task.id))
            }
            other => Err(format!("不支持的 action: {other}")),
        }
    }
}

/// 绑定 `binding` 聊天的 Schedule 工具，`channel` 为渠道名
pub(super) fn tool(
    store: Arc<ScheduleStore>,
    channel: String,
    binding: String,
    session_id: String,
) -> Arc<dyn Tool> {
    Arc::new(ScheduleTool {
        store,
        channel,
        binding,
        session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(dir: &std::path::Path) -> (ScheduleTool, Arc<ScheduleStore>) {
        let store = ScheduleStore::open(dir).unwrap();
        let tool = ScheduleTool {
            store: store.clone(),
            channel: "qq".into(),
            binding: "qq:c2c:u1".into(),
            session_id: "sess_abc".into(),
        };
        (tool, store)
    }

    #[tokio::test]
    async fn create_list_pause_resume_delete() {
        let dir = tempfile::tempdir().unwrap();
        let (tool, store) = tool(dir.path());
        let out = tool
            .execute(r#"{"action":"create","id":"morning","cron":"0 9 * * *","prompt":"汇总待办","bindings":["qq:c2c:u1","qq:group:g1"]}"#)
            .await
            .unwrap();
        assert!(
            out.contains("morning") && out.contains("qq:c2c:u1") && out.contains("home"),
            "{out}"
        );
        assert_eq!(
            store.get("morning").unwrap().0.bindings,
            ["qq:c2c:u1", "qq:group:g1"]
        );
        // 新执行会话缺省使用当前聊天为来源。
        tool.execute(r#"{"action":"create","id":"w","every":"1h","prompt":"喝水"}"#)
            .await
            .unwrap();
        let w = store.get("w").unwrap().0;
        assert_eq!(w.bindings, ["qq:c2c:u1"]);
        // target=session：在当前会话内执行
        tool.execute(
            r#"{"action":"create","id":"s","every":"20m","prompt":"看结果","target":"session"}"#,
        )
        .await
        .unwrap();
        let s = store.get("s").unwrap().0;
        assert_eq!(s.session.as_deref(), Some("sess_abc"));
        assert!(s.bindings.is_empty());

        assert!(
            tool.execute(r#"{"action":"list"}"#)
                .await
                .unwrap()
                .contains("当前时间")
        );
        tool.execute(r#"{"action":"pause","id":"mor"}"#)
            .await
            .unwrap();
        assert!(!store.list()[0].1.enabled);
        tool.execute(r#"{"action":"resume","id":"morning"}"#)
            .await
            .unwrap();
        tool.execute(r#"{"action":"delete","id":"w"}"#)
            .await
            .unwrap();
        assert_eq!(store.list().len(), 2);
    }

    #[tokio::test]
    async fn invalid_arguments_are_explicit_errors() {
        let dir = tempfile::tempdir().unwrap();
        let (tool, _) = tool(dir.path());
        for bad in [
            r#"{"action":"create","prompt":"p"}"#,
            r#"{"action":"create","cron":"* * * * *","every":"1h","prompt":"p"}"#,
            r#"{"action":"create","cron":"bad","prompt":"p"}"#,
            r#"{"action":"create","cron":"* * * * *"}"#,
            r#"{"action":"create","cron":"* * * * *","prompt":"p","bindings":["tg:1"]}"#,
            r#"{"action":"create","cron":"* * * * *","prompt":"p","target":"x"}"#,
            r#"{"action":"pause"}"#,
            r#"{"action":"nope"}"#,
        ] {
            assert!(tool.execute(bad).await.is_err(), "{bad}");
        }
        let err = tool
            .execute(r#"{"action":"create","at":"2000-01-01T00:00","prompt":"p"}"#)
            .await
            .unwrap_err();
        assert!(err.contains("当前时间"), "{err}");
    }
}
