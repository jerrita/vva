use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ContentPart, Session};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
    Stopped,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundTask {
    pub task_id: String,
    pub status: BackgroundTaskStatus,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub truncated: bool,
}

impl BackgroundTask {
    pub fn running(task_id: String) -> Self {
        Self {
            task_id,
            status: BackgroundTaskStatus::Running,
            output: String::new(),
            exit_code: None,
            total_bytes: None,
            truncated: false,
        }
    }

    pub fn label(&self) -> String {
        match self.status {
            BackgroundTaskStatus::Running => "后台运行中".into(),
            BackgroundTaskStatus::Completed => "已完成".into(),
            BackgroundTaskStatus::Failed => self
                .exit_code
                .map_or_else(|| "失败".into(), |code| format!("退出码 {code}")),
            BackgroundTaskStatus::Stopped => "已停止".into(),
            BackgroundTaskStatus::Interrupted => "已中断".into(),
        }
    }
}

/// 同一任务终态不被较晚提交的启动回执覆盖；主动停止保留其语义。
pub fn update_background_task(
    tasks: &mut BTreeMap<String, BackgroundTask>,
    mut task: BackgroundTask,
) {
    if let Some(previous) = tasks.get(&task.task_id)
        && previous.status != BackgroundTaskStatus::Running
        && task.status == BackgroundTaskStatus::Running
    {
        return;
    }
    if let Some(previous) = tasks.get(&task.task_id)
        && previous.status == BackgroundTaskStatus::Stopped
    {
        task.status = BackgroundTaskStatus::Stopped;
        if task.output.is_empty() {
            task.output.clone_from(&previous.output);
        }
    }
    tasks.insert(task.task_id.clone(), task);
}

/// 仅投影仍有启动调用的任务，回退后到达的结果不会重新引入旧任务。
pub fn background_tasks(session: &Session) -> BTreeMap<String, BackgroundTask> {
    let mut tasks = BTreeMap::new();
    for turn in &session.turns {
        for part in turn.messages.iter().flat_map(|message| &message.parts) {
            if let ContentPart::ToolResult(result) = part {
                if let Some(id) = &result.background_task_id {
                    tasks
                        .entry(id.clone())
                        .or_insert_with(|| BackgroundTask::running(id.clone()));
                }
                if let Some(task) = &result.task {
                    update_background_task(&mut tasks, task.clone());
                }
            }
        }
        if let Some(task) = &turn.background_task {
            update_background_task(&mut tasks, task.clone());
        }
    }
    let launched: std::collections::BTreeSet<_> = session
        .turns
        .iter()
        .flat_map(|turn| &turn.messages)
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            ContentPart::ToolResult(result) => result.background_task_id.as_ref(),
            _ => None,
        })
        .collect();
    tasks.retain(|id, _| launched.contains(id));
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, Role, ToolResult, Turn};

    #[test]
    fn terminal_state_survives_late_launch_receipt_and_stop_preserves_output() {
        let mut tasks = BTreeMap::new();
        let task = BackgroundTask {
            status: BackgroundTaskStatus::Completed,
            output: "ready".into(),
            ..BackgroundTask::running("task_sample".into())
        };
        update_background_task(&mut tasks, task.clone());
        update_background_task(&mut tasks, BackgroundTask::running(task.task_id.clone()));
        assert_eq!(tasks[&task.task_id], task);
        update_background_task(
            &mut tasks,
            BackgroundTask {
                status: BackgroundTaskStatus::Stopped,
                ..task.clone()
            },
        );
        update_background_task(&mut tasks, task.clone());
        assert_eq!(tasks[&task.task_id].status, BackgroundTaskStatus::Stopped);
        assert_eq!(tasks[&task.task_id].output, "ready");
    }

    #[test]
    fn append_only_snapshots_roundtrip_and_unowned_completions_are_excluded() {
        let mut session = Session::new("/synthetic", None);
        let running = BackgroundTask::running("task_sample".into());
        let mut result = ToolResult::success("call_sample", "started");
        result.background_task_id = Some(running.task_id.clone());
        result.task = Some(running.clone());
        session.append_turn(Turn::new(Role::Tool, vec![Message::tool_result(result)]));
        let mut completion = Turn::user_turn(Message::text("machine delivery"));
        completion.background_task = Some(BackgroundTask {
            status: BackgroundTaskStatus::Completed,
            output: "sample output".into(),
            ..running.clone()
        });
        session.append_turn(completion);
        let mut loaded = Session::new("/synthetic", None);
        loaded.turns =
            serde_json::from_str(&serde_json::to_string(&session.turns).unwrap()).unwrap();
        assert_eq!(
            background_tasks(&loaded)[&running.task_id].output,
            "sample output"
        );
        session.turns.remove(0);
        assert!(background_tasks(&session).is_empty());
    }
}
