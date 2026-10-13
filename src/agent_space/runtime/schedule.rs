//! 定时任务执行：触发去重、执行目标解析、超时中止、结果回投与失败通知。

use std::sync::Arc;
use std::time::Duration;

use super::{DeliveryTarget, EMPTY_REPLY, Runtime};
use crate::agent_space::scheduler::{Failure, Missed, Task, Trigger, format_time};
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

    async fn execute(self: &Arc<Self>, task: &Task) -> Result<(), Failure> {
        tracing::info!(
            "定时任务触发: id={} {} session={:?} bindings={:?}",
            task.id,
            task.trigger.describe(),
            task.session,
            task.bindings
        );
        // 执行会话仍取 session 或 bindings[0]；通知单独投递到 home。
        let handle = match &task.session {
            Some(id) => {
                let handle =
                    self.manager.live_or_load(id).await.map_err(|e| {
                        Failure::Permanent(format!("目标 session {id} 不可用: {e}"))
                    })?;
                let origin = session_binding(&handle).map_err(Failure::Permanent)?;
                self.origin(&origin).map_err(Failure::Permanent)?;
                handle
            }
            None => {
                let binding = task
                    .bindings
                    .first()
                    .ok_or_else(|| Failure::Permanent("任务没有执行目标".into()))?;
                self.origin(binding).map_err(Failure::Permanent)?;
                self.create(binding, SessionKind::Oneshot, "scheduled")
                    .await
                    .map_err(Failure::Transient)?
            }
        };
        let prompt = format_scheduled_reminder(&task.id, &task.trigger.describe(), &task.prompt);
        let limit = Duration::from_secs(self.config.task_timeout_secs);
        let report = match tokio::time::timeout(
            limit,
            self.deliver_reporting(&handle, DeliveryTarget::Homes, &prompt),
        )
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
            self.send_report(DeliveryTarget::Homes, text, Some(&handle.id))
                .await;
        }
        self.after_turn(&handle, DeliveryTarget::Homes).await;
        Ok(())
    }

    /// 记录结果；失败时通知 home bindings
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
        self.send_report(DeliveryTarget::Homes, &text, None).await;
    }

    /// 停机期间错过的任务：汇总通知 home bindings，不补跑
    pub(super) async fn report_missed(&self, missed: Vec<Missed>) {
        let mut lines = Vec::new();
        for m in missed {
            let last = format_time(m.last);
            let line = match m.task.trigger {
                Trigger::Once(_) => format!(
                    "{}（应于 {last}，已错过；可 /schedule run {} 补跑）",
                    m.task.id, m.task.id
                ),
                _ => format!("{}（最近应于 {last}，未补跑）", m.task.id),
            };
            lines.push(line);
        }
        if !lines.is_empty() {
            let text = format!("停机期间错过了以下定时任务: {}", lines.join("；"));
            self.send_report(DeliveryTarget::Homes, &text, None).await;
        }
    }
}

/// 会话来源 binding，用于校验既有执行会话的渠道。
fn session_binding(handle: &ManagedSession) -> Result<String, String> {
    let snapshot = handle.snapshot();
    let origin = snapshot
        .session
        .header
        .origin
        .as_ref()
        .ok_or_else(|| format!("session {} 无 origin，无法执行定时任务", handle.id))?;
    Ok(origin.binding())
}
