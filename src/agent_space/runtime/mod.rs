//! Daemon 运行时：入站消息 → 命令 / 文件缓冲 → 路由 → session 轮次 → 出站文本。
//!
//! 路由与会话创建在入站循环内顺序完成（保证同一 binding 的决策有序），
//! 轮次执行与回复在独立任务中进行，因此运行中的 session 仍可接收插话。
//! 本期只回最终文本，不做流式增量发送。

mod commands;
mod inbound;
mod mode_model;
mod schedule;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{Mutex, mpsc};

use self::mode_model::{ModeModel, load_mode_model};
use super::binding::{BindingStore, MessageIndex, PendingFiles};
use super::channel::{Channel, Inbound, Outbound, OutboundText};
use super::curator::{self, Curator, CuratorReport, CuratorStatus, ModelSource};
use super::progress;
use super::rollover::maybe_rollover;
use super::router::Router;
use super::scheduler::{self, ScheduleStore};
use crate::config::AgentSpaceConfig;
use crate::harness::{ManagedSession, SessionManager};
use crate::model::{Origin, Session, SessionKind, SessionSummary};
use crate::tools::agent::SubagentReport;

/// 待附件缓冲的过期检查周期
const EXPIRE_PERIOD: Duration = Duration::from_secs(30);
/// 轮次没有文本输出时的回复
const EMPTY_REPLY: &str = "（本轮没有文本输出）";

/// 用户轮次回投发起聊天；后台执行在发送时查询 home 集合。
#[derive(Clone, Copy)]
enum DeliveryTarget<'a> {
    Chat(&'a str),
    Homes,
}

struct State {
    bindings: BindingStore,
    index: MessageIndex,
    pending: PendingFiles,
}

pub struct Runtime {
    manager: Arc<SessionManager>,
    config: AgentSpaceConfig,
    channel: Arc<dyn Channel>,
    router: Router,
    curator: Arc<Curator>,
    schedule: Arc<ScheduleStore>,
    /// 正在执行的定时任务 id（同一任务不重叠）
    running_tasks: std::sync::Mutex<HashSet<String>>,
    /// 正在汇报进度的会话 id（同一会话仅一个汇报者）
    reporting: std::sync::Mutex<HashSet<String>>,
    /// agent 模式的模型（新会话使用）；`/model --global` 修改并落盘
    mode_model: Arc<std::sync::Mutex<ModeModel>>,
    state: Mutex<State>,
}

impl Runtime {
    pub fn new(
        manager: Arc<SessionManager>,
        config: AgentSpaceConfig,
        channel: Arc<dyn Channel>,
        router: Router,
        schedule: Arc<ScheduleStore>,
    ) -> Result<Arc<Self>, String> {
        let dir = manager.workspace();
        let state = State {
            bindings: BindingStore::load(dir)?,
            index: MessageIndex::load(dir)?,
            pending: PendingFiles::default(),
        };
        let mode_model = Arc::new(std::sync::Mutex::new(load_mode_model(&config, &manager)?));
        let source: ModelSource = {
            let mode_model = mode_model.clone();
            Arc::new(move || mode_model.lock().expect("mode_model").model.clone())
        };
        let curator = Arc::new(Curator::for_manager(&manager, source));
        Ok(Arc::new(Self {
            manager,
            config,
            channel,
            router,
            curator,
            schedule,
            running_tasks: Default::default(),
            reporting: Default::default(),
            mode_model,
            state: Mutex::new(state),
        }))
    }

    pub fn curator(&self) -> Arc<Curator> {
        self.curator.clone()
    }

    /// 主循环：入站消息、定时事件与待附件过期；入站通道关闭时返回
    pub async fn run(
        self: Arc<Self>,
        mut inbound: mpsc::Receiver<Inbound>,
        mut events: mpsc::Receiver<scheduler::Event>,
    ) {
        let mut expire = tokio::time::interval(EXPIRE_PERIOD);
        loop {
            tokio::select! {
                msg = inbound.recv() => match msg {
                    Some(msg) => self.handle(msg).await,
                    None => return,
                },
                Some(event) = events.recv() => match event {
                    scheduler::Event::Fire(task) => {
                        self.fire(task).await;
                    }
                    scheduler::Event::Missed(missed) => self.report_missed(missed).await,
                },
                _ = expire.tick() => self.expire_pending().await,
            }
        }
    }

    // ================== 轮次与回复 ==================

    /// 投递并等待一轮，回复最终文本；随后检查 rollover
    async fn run_turn(
        self: Arc<Self>,
        handle: Arc<ManagedSession>,
        binding: String,
        reply_to: String,
        prompt: String,
    ) {
        tracing::info!(
            "Deliver 开始: session={} binding={binding} prompt {} 字符",
            handle.id,
            prompt.chars().count()
        );
        let started = std::time::Instant::now();
        let turns_before = handle.snapshot().session.turns.len();
        let delivered = self
            .deliver_reporting(&handle, DeliveryTarget::Chat(&binding), &prompt)
            .await;
        let (prompt_tokens, completion_tokens, cached_tokens) =
            turn_usage(&handle.snapshot().session, turns_before);
        tracing::info!(
            "Deliver 结束: session={} 轮次={} 耗时 {:.1}s prompt/completion/cached tokens={prompt_tokens}/{completion_tokens}/{cached_tokens} 出错={}",
            handle.id,
            delivered.as_ref().map_or(0, |r| r.turn_seq),
            started.elapsed().as_secs_f32(),
            delivered.is_err()
        );
        let text = match delivered {
            Ok(report) if report.text.trim().is_empty() => EMPTY_REPLY.to_string(),
            Ok(report) => report.text,
            Err(error) => {
                tracing::warn!("会话 {} 本轮执行失败: {error}", handle.id);
                format!("本轮执行失败: {error}")
            }
        };
        self.say_indexed(&binding, Some(&reply_to), &text, &handle.id)
            .await;
        self.after_turn(&handle, DeliveryTarget::Chat(&binding))
            .await;
    }

    /// 投递并等待一轮；轮次超过 `progress_interval_secs` 未结束时向目标推送进度摘要。
    /// 同一会话同时只有一个汇报者（排队的后续投递不重复汇报）；汇报消息计入消息索引，可被 reply 直达该会话
    async fn deliver_reporting(
        &self,
        handle: &Arc<ManagedSession>,
        target: DeliveryTarget<'_>,
        prompt: &str,
    ) -> Result<SubagentReport, String> {
        let interval = Duration::from_secs(self.config.progress_interval_secs);
        if interval.is_zero()
            || !self
                .reporting
                .lock()
                .expect("reporting")
                .insert(handle.id.clone())
        {
            return handle.deliver_wait(prompt).await;
        }
        // 调用方超时会丢弃本 future，汇报者登记须随 drop 释放
        let _reporter = ReporterGuard {
            reporting: &self.reporting,
            id: &handle.id,
        };
        progress::with_progress(
            handle.deliver_wait(prompt),
            interval,
            || handle.snapshot().session.clone(),
            |text| async move {
                self.send_report(target, &text, Some(&handle.id)).await;
            },
        )
        .await
    }

    async fn after_turn(&self, handle: &Arc<ManagedSession>, target: DeliveryTarget<'_>) {
        let text = match maybe_rollover(&self.manager, &self.config, handle).await {
            Ok(None) => return,
            Ok(Some(rolled)) => {
                let moved = {
                    let mut state = self.state.lock().await;
                    state
                        .bindings
                        .replace_focus(&rolled.old_id, Some(&rolled.new_id))
                        .and_then(|()| state.index.repoint(&rolled.old_id, &rolled.new_id))
                };
                if let Err(e) = moved {
                    tracing::warn!("rollover 后切换指针失败: {e}");
                }
                format!("上下文接近上限，已压缩并切换到新会话 {}", rolled.new_id)
            }
            Err(error) => {
                tracing::warn!("会话 {} rollover 失败: {error}", handle.id);
                format!("上下文压缩失败，下个回合结束重试: {error}")
            }
        };
        self.send_report(target, &text, None).await;
    }

    // ================== 出站与推送 ==================

    /// 发送文本，返回渠道消息 id；发送失败只记日志
    async fn say(
        &self,
        binding: &str,
        reply_to: Option<&str>,
        text: impl Into<String>,
    ) -> Vec<String> {
        let text = text.into();
        tracing::debug!("出站回复: binding={binding} {} 字符", text.chars().count());
        let msg = Outbound::Text(OutboundText {
            binding: binding.to_string(),
            text,
            reply_to: reply_to.map(str::to_string),
        });
        self.channel.send(msg).await.unwrap_or_else(|e| {
            tracing::warn!("发送到 {binding} 失败: {e}");
            Vec::new()
        })
    }

    /// 发送会话产出的文本，并把出站消息 id 记入消息索引（用户 reply 可直达 `session`）
    async fn say_indexed(&self, binding: &str, reply_to: Option<&str>, text: &str, session: &str) {
        let ids = self.say(binding, reply_to, text).await;
        let mut state = self.state.lock().await;
        for id in ids {
            if let Err(e) = state.index.record(&self.key(&id), session) {
                tracing::warn!("记录出站消息索引失败: {e}");
            }
        }
    }

    /// 后台通知每次查询最新 home 集合；无 home 时只记日志，执行照常继续。
    async fn send_report(&self, target: DeliveryTarget<'_>, text: &str, session: Option<&str>) {
        let bindings = match target {
            DeliveryTarget::Chat(binding) => vec![binding.to_string()],
            DeliveryTarget::Homes => self.state.lock().await.bindings.homes(),
        };
        if bindings.is_empty() {
            tracing::info!("后台通知未推送：尚未设置 home binding");
        }
        for binding in bindings {
            if matches!(target, DeliveryTarget::Homes)
                && !self.state.lock().await.bindings.is_home(&binding)
            {
                continue;
            }
            if let Some(session) = session {
                self.say_indexed(&binding, None, text, session).await;
            } else {
                self.say(&binding, None, text).await;
            }
        }
    }

    /// Lifecycle 整理完成回调：有更新或失败时推送到 home bindings
    pub async fn push_curator(&self, report: CuratorReport) {
        let text = match report.result.status {
            CuratorStatus::Unchanged => return,
            CuratorStatus::Updated => format!(
                "记忆已整理（{}）: {}",
                report.session_id, report.result.summary
            ),
            CuratorStatus::Failed => format!(
                "记忆整理失败（{}）: {}",
                report.session_id, report.result.summary
            ),
        };
        tracing::info!(
            "推送 curator 报告: session={} status={:?}",
            report.session_id,
            report.result.status
        );
        if report.result.status == CuratorStatus::Updated {
            self.broadcast_memory_update(&report).await;
        }
        self.send_report(DeliveryTarget::Homes, &text, None).await;
    }

    /// 记忆已更新：向现存 topic / oneshot 会话追加 `memory-updated` 提示（只追加，不起轮次）。
    /// 排除 curator 会话、已被接替的会话与来源会话（即将删除）；单个会话失败只告警
    async fn broadcast_memory_update(&self, report: &CuratorReport) {
        let summaries = match self.manager.store().list_sessions().await {
            Ok(summaries) => summaries,
            Err(error) => {
                tracing::warn!("记忆更新广播无法列出会话: {error}");
                return;
            }
        };
        let text = crate::system::format_memory_updated_reminder(
            &curator::memory_dir(self.manager.workspace()),
            &report.result.summary,
        );
        let (mut total, mut failed) = (0, 0);
        for s in summaries.into_iter().filter(|s| {
            self.is_active_space_session(s)
                && s.kind != SessionKind::Curator
                && s.id != report.session_id
        }) {
            total += 1;
            let delivered = match self.manager.live_or_load(&s.id).await {
                Ok(handle) => handle.note(&text).await,
                Err(error) => Err(error),
            };
            if let Err(error) = delivered {
                failed += 1;
                tracing::warn!("向会话 {} 追加记忆更新提示失败: {error}", s.id);
            }
        }
        tracing::info!("记忆更新广播: 目标会话 {total} 个, 失败 {failed} 个");
    }

    /// 待附件超时：提示用户并清理缓冲（文件仍保留在磁盘）
    async fn expire_pending(&self) {
        let ttl = chrono::Duration::seconds(self.config.pending_files_ttl_secs as i64);
        let expired = self.state.lock().await.pending.expire(Utc::now(), ttl);
        for (binding, files) in expired {
            tracing::debug!("待附件过期: binding={binding} {} 个", files.len());
            let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
            self.say(
                &binding,
                None,
                format!(
                    "等待超时，{} 个待处理文件已不再关联: {}",
                    files.len(),
                    names.join("、")
                ),
            )
            .await;
        }
    }

    // ================== 标识 ==================

    /// 本工作区内未被接替的 agent space 会话
    fn is_active_space_session(&self, s: &SessionSummary) -> bool {
        s.agent_managed()
            && s.workspace_root == self.manager.workspace()
            && s.superseded_by.is_none()
    }

    /// 索引键：带 channel 前缀，避免跨渠道 message_id 冲突
    fn key(&self, message_id: &str) -> String {
        format!("{}:{message_id}", self.channel.name())
    }

    /// binding `channel:chat_id` → session 来源；channel 须与当前渠道一致
    fn origin(&self, binding: &str) -> Result<Origin, String> {
        let (channel, chat_id) = binding
            .split_once(':')
            .ok_or_else(|| format!("非法 binding: {binding}"))?;
        if channel != self.channel.name() {
            return Err(format!(
                "binding {binding} 不属于当前渠道 {}",
                self.channel.name()
            ));
        }
        Ok(Origin {
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
        })
    }
}

/// 进度汇报者登记：drop 时移除
struct ReporterGuard<'a> {
    reporting: &'a std::sync::Mutex<HashSet<String>>,
    id: &'a str,
}

impl Drop for ReporterGuard<'_> {
    fn drop(&mut self) {
        self.reporting.lock().expect("reporting").remove(self.id);
    }
}

/// 自 `from` 之后新增 assistant 轮次的 (prompt, completion, cached) token 合计
fn turn_usage(session: &Session, from: usize) -> (u64, u64, u64) {
    session
        .turns
        .iter()
        .skip(from)
        .filter_map(|t| t.usage.as_ref())
        .fold((0, 0, 0), |acc, u| {
            (
                acc.0 + u.prompt_tokens as u64,
                acc.1 + u.completion_tokens as u64,
                acc.2 + u.cached_tokens.unwrap_or_default() as u64,
            )
        })
}
