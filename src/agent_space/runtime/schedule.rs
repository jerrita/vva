//! 定时任务执行：触发去重、执行目标解析、超时中止、结果回投与失败通知。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use super::{EMPTY_REPLY, Runtime, origin_binding};
use crate::agent_space::scheduler::{Failure, Missed, Task, Trigger};
use crate::harness::ManagedSession;
use crate::model::SessionKind;
use crate::system::format_scheduled_reminder;

/// 定时任务回复此标记表示无需通知
const SILENT: &str = "[SILENT]";

impl Runtime {
    /// 启动一次任务执行；同一任务仍在执行时跳过并返回 `false`
    pub(super) async fn fire(self: &Arc<Self>, task: Task) -> bool {
        if !self
            .running_tasks
            .lock()
            .expect("running_tasks")
            .insert(task.id.clone())
        {
            tracing::debug!("定时任务 {} 上次未结束，跳过本次", task.id);
            return false;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let outcome = this.execute(&task).await;
            this.running_tasks
                .lock()
                .expect("running_tasks")
                .remove(&task.id);
            this.settle(&task, outcome).await;
        });
        true
    }

    /// 任务回复的发送目标：session 来源（若可用）+ bindings，去重保序
    async fn task_targets(&self, task: &Task) -> Vec<String> {
        let origin = match &task.session {
            Some(id) => match self.manager.live_or_load(id).await {
                Ok(handle) => session_binding(&handle).ok(),
                Err(_) => None,
            },
            None => None,
        };
        merge_targets(origin, &task.bindings)
    }

    async fn execute(self: &Arc<Self>, task: &Task) -> Result<(), Failure> {
        tracing::info!(
            "定时任务触发: id={} {} session={:?} bindings={:?}",
            task.id,
            task.trigger.describe(),
            task.session,
            task.bindings
        );
        // 首个目标即执行会话的来源 binding（rollover 提示发往此处）
        let (handle, targets) = match &task.session {
            Some(id) => {
                let handle =
                    self.manager.live_or_load(id).await.map_err(|e| {
                        Failure::Permanent(format!("目标 session {id} 不可用: {e}"))
                    })?;
                let origin = session_binding(&handle).map_err(Failure::Permanent)?;
                (handle, merge_targets(Some(origin), &task.bindings))
            }
            None => {
                let binding = task
                    .bindings
                    .first()
                    .ok_or_else(|| Failure::Permanent("任务没有执行目标".into()))?;
                self.origin(binding).map_err(Failure::Permanent)?;
                let handle = self
                    .create(binding, SessionKind::Oneshot, "scheduled")
                    .await
                    .map_err(Failure::Transient)?;
                (handle, merge_targets(None, &task.bindings))
            }
        };
        let prompt = format_scheduled_reminder(&task.id, &task.trigger.describe(), &task.prompt);
        let limit = Duration::from_secs(self.config.task_timeout_secs);
        let report =
            match tokio::time::timeout(limit, self.deliver_reporting(&handle, &targets, &prompt))
                .await
            {
                Ok(result) => result.map_err(Failure::Transient)?,
                Err(_) => {
                    if let Err(error) = handle.abort_current().await {
                        tracing::warn!("定时任务 {} 超时中止失败: {error}", task.id);
                    }
                    return Err(Failure::Transient(format!(
                        "执行超过 {}s，已中止",
                        self.config.task_timeout_secs
                    )));
                }
            };
        let text = report.text.trim();
        if text == SILENT {
            tracing::info!("定时任务 {} 回复 {SILENT}，不通知", task.id);
        } else {
            let text = if text.is_empty() { EMPTY_REPLY } else { text };
            for target in &targets {
                self.say_indexed(target, None, text, &handle.id).await;
            }
        }
        self.after_turn(&handle, &targets[0]).await;
        Ok(())
    }

    /// 记录结果；失败时通知所有目标聊天
    async fn settle(&self, task: &Task, outcome: Result<(), Failure>) {
        let max = self.config.task_max_failures;
        let Some(settled) = self.schedule.settle(&task.id, outcome.clone(), max) else {
            return;
        };
        let Err(Failure::Permanent(msg) | Failure::Transient(msg)) = outcome else {
            return;
        };
        tracing::warn!("定时任务 {} 执行失败: {msg}", task.id);
        let text = if settled.disabled {
            format!(
                "⚠️ 定时任务 {} 执行失败并已停用: {msg}（修复后可 /schedule resume {}）",
                task.id, task.id
            )
        } else {
            format!(
                "⚠️ 定时任务 {} 执行失败（连续 {}/{max} 次，达到上限将停用）: {msg}",
                task.id, settled.failures
            )
        };
        for target in self.task_targets(task).await {
            self.say(&target, None, text.clone()).await;
        }
    }

    /// 停机期间错过的任务：按聊天汇总提示一次，不补跑
    pub(super) async fn report_missed(&self, missed: Vec<Missed>) {
        let mut by_binding: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for m in missed {
            let last = m.last.format("%Y-%m-%d %H:%M");
            let line = match m.task.trigger {
                Trigger::Once(_) => format!(
                    "{}（应于 {last}，已错过；可 /schedule run {} 补跑）",
                    m.task.id, m.task.id
                ),
                _ => format!("{}（最近应于 {last}，未补跑）", m.task.id),
            };
            let targets = self.task_targets(&m.task).await;
            if targets.is_empty() {
                tracing::warn!("错过的定时任务 {} 无法通知: 没有可用目标", m.task.id);
            }
            for binding in targets {
                by_binding.entry(binding).or_default().push(line.clone());
            }
        }
        for (binding, lines) in by_binding {
            self.say(
                &binding,
                None,
                format!("停机期间错过了以下定时任务: {}", lines.join("；")),
            )
            .await;
        }
    }
}

/// 会话来源 binding；无来源的会话无法回复
fn session_binding(handle: &ManagedSession) -> Result<String, String> {
    let snapshot = handle.snapshot();
    let origin = snapshot
        .session
        .header
        .origin
        .as_ref()
        .ok_or_else(|| format!("session {} 无 origin，无法回复", handle.id))?;
    Ok(origin_binding(origin))
}

/// 会话来源（若有）在前，随后 bindings，去重保序
fn merge_targets(origin: Option<String>, bindings: &[String]) -> Vec<String> {
    let mut targets: Vec<String> = origin.into_iter().collect();
    for binding in bindings {
        if !targets.contains(binding) {
            targets.push(binding.clone());
        }
    }
    targets
}
