//! Daemon 运行时：入站消息 → 命令 / 文件缓冲 → 路由 → session 轮次 → 出站文本。
//!
//! 路由与会话创建在入站循环内顺序完成（保证同一 binding 的决策有序），
//! 轮次执行与回复在独立任务中进行，因此运行中的 session 仍可接收插话。
//! 本期只回最终文本，不做流式增量发送。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{Mutex, mpsc};

use super::agent_toggles;
use super::binding::{AttachedFile, BindingStore, FileKind, MessageIndex, PendingFiles};
use super::channel::files::{sanitize_filename, unique_dest};
use super::channel::{Channel, Inbound, Outbound, OutboundText};
use super::command::{self, Command};
use super::curator::{CuratorReport, CuratorStatus};
use super::rollover::{cache_hit_rate, context_tokens, maybe_rollover};
use super::router::{Candidate, ReplyRef, Route, RouteInput, Routed, Router, resolve_prefix};
use super::scheduler::{self, Missed, Target, Task};
use crate::config::AgentSpaceConfig;
use crate::harness::{ManagedSession, SessionManager};
use crate::model::{Origin, Session, SessionKind};
use crate::system::format_scheduled_reminder;

/// 待附件缓冲的过期检查周期
const EXPIRE_PERIOD: Duration = Duration::from_secs(30);

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
    state: Mutex<State>,
}

impl Runtime {
    pub fn new(
        manager: Arc<SessionManager>,
        config: AgentSpaceConfig,
        channel: Arc<dyn Channel>,
        router: Router,
    ) -> Result<Arc<Self>, String> {
        let dir = manager.workspace();
        let state = State {
            bindings: BindingStore::load(dir)?,
            index: MessageIndex::load(dir)?,
            pending: PendingFiles::default(),
        };
        Ok(Arc::new(Self {
            manager,
            config,
            channel,
            router,
            state: Mutex::new(state),
        }))
    }

    /// topic 与 oneshot session 使用的模型规格
    pub fn model(&self) -> String {
        self.config
            .model
            .clone()
            .unwrap_or_else(|| self.manager.app_config().settings.default.clone())
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
                    scheduler::Event::Fire(task) => self.fire(task).await,
                    scheduler::Event::Missed(missed) => self.report_missed(missed).await,
                },
                _ = expire.tick() => self.expire_pending().await,
            }
        }
    }

    // ================== 入站 ==================

    /// 处理一条入站消息；失败原样回复用户
    pub async fn handle(self: &Arc<Self>, msg: Inbound) {
        if let Err(error) = self.dispatch(&msg).await {
            self.say(&msg.binding, Some(&msg.message_id), error).await;
        }
    }

    async fn dispatch(self: &Arc<Self>, msg: &Inbound) -> Result<(), String> {
        if let Some(cmd) = command::parse(&msg.text) {
            return self.command(msg, cmd).await;
        }
        let own = self.save_files(msg).await?;
        if msg.text.trim().is_empty() {
            return self.buffer_files(msg, own).await;
        }
        let mut files = self.state.lock().await.pending.take(&msg.binding);
        files.extend(own);
        self.route_and_run(msg, msg.text.trim().to_string(), files, false)
            .await
    }

    /// 附件落盘到 `<agent_dir>/files/<binding 清洗>/`
    async fn save_files(&self, msg: &Inbound) -> Result<Vec<AttachedFile>, String> {
        if msg.files.is_empty() {
            return Ok(Vec::new());
        }
        let dir = self
            .manager
            .workspace()
            .join("files")
            .join(sanitize_filename(&msg.binding));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
        let mut saved = Vec::new();
        for file in &msg.files {
            let path = unique_dest(&dir, &file.filename).await;
            file.download(&path)
                .await
                .map_err(|e| format!("下载附件 {} 失败: {e}", file.filename))?;
            saved.push(AttachedFile {
                path,
                name: file.filename.clone(),
                kind: if file.mime.starts_with("image/") {
                    FileKind::Image
                } else {
                    FileKind::File
                },
            });
        }
        Ok(saved)
    }

    /// 无文字文件只缓冲，不触发路由
    async fn buffer_files(&self, msg: &Inbound, files: Vec<AttachedFile>) -> Result<(), String> {
        let now = Utc::now();
        let count = {
            let mut state = self.state.lock().await;
            for file in files {
                state.pending.push(&msg.binding, file, now);
            }
            state.pending.count(&msg.binding)
        };
        self.say(
            &msg.binding,
            Some(&msg.message_id),
            format!("已收到，当前待处理文件 {count} 个。请发送文字说明，我会连同文件一起处理"),
        )
        .await;
        Ok(())
    }

    async fn route_and_run(
        self: &Arc<Self>,
        msg: &Inbound,
        text: String,
        files: Vec<AttachedFile>,
        force_new: bool,
    ) -> Result<(), String> {
        let routed = if force_new {
            Routed {
                route: Route::New {
                    quoted: msg.reply_to.as_ref().map(|r| r.text.clone()),
                },
                routed_by: "new",
                warning: None,
            }
        } else {
            self.route(msg, &text, &files).await?
        };
        if let Some(warning) = &routed.warning {
            self.say(&msg.binding, Some(&msg.message_id), warning).await;
        }
        let (handle, prompt, focus) = match routed.route {
            Route::Existing(id) => (
                self.manager.live_or_load(&id).await?,
                compose_prompt(&text, &files, None),
                true,
            ),
            Route::Guide(id) => {
                self.say(&msg.binding, Some(&msg.message_id), "已转达给运行中的会话")
                    .await;
                (
                    self.manager.live_or_load(&id).await?,
                    compose_prompt(&text, &files, None),
                    true,
                )
            }
            Route::New { quoted } => (
                self.create(&msg.binding, SessionKind::Topic, routed.routed_by)
                    .await?,
                compose_prompt(&text, &files, quoted.as_deref()),
                true,
            ),
            // oneshot 不进候选，也不抢占 focus
            Route::Oneshot => (
                self.create(&msg.binding, SessionKind::Oneshot, routed.routed_by)
                    .await?,
                compose_prompt(&text, &files, None),
                false,
            ),
        };
        {
            let mut state = self.state.lock().await;
            if focus {
                state
                    .bindings
                    .set_focus(&msg.binding, Some(handle.id.clone()))?;
            }
            state.index.record(&self.key(&msg.message_id), &handle.id)?;
        }
        tokio::spawn(self.clone().run_turn(
            handle,
            msg.binding.clone(),
            Some(msg.message_id.clone()),
            prompt,
        ));
        Ok(())
    }

    async fn route(
        &self,
        msg: &Inbound,
        text: &str,
        files: &[AttachedFile],
    ) -> Result<Routed, String> {
        let input = RouteInput {
            binding: msg.binding.clone(),
            text: text.to_string(),
            reply_to: msg.reply_to.as_ref().map(|r| ReplyRef {
                message_id: self.key(&r.message_id),
                text: r.text.clone(),
            }),
            files: files.to_vec(),
            candidates: self.candidates(&msg.binding).await?,
        };
        let state = self.state.lock().await;
        self.router
            .route(&input, &state.index)
            .await
            .map_err(|e| e.to_string())
    }

    /// router 候选：排除 oneshot / curator / 已 superseded 与非 agent space 会话；
    /// 顺带清理已不存在的 focus
    async fn candidates(&self, binding: &str) -> Result<Vec<Candidate>, String> {
        let summaries = self
            .manager
            .store()
            .list_sessions()
            .await
            .map_err(|e| e.to_string())?;
        let mut focus = self
            .state
            .lock()
            .await
            .bindings
            .focus(binding)
            .map(str::to_string);
        if focus
            .as_ref()
            .is_some_and(|f| !summaries.iter().any(|s| &s.id == f))
        {
            self.state.lock().await.bindings.set_focus(binding, None)?;
            focus = None;
        }
        let mut candidates = Vec::new();
        for s in summaries.into_iter().filter(|s| {
            s.agent_managed()
                && s.workspace_root == self.manager.workspace()
                && s.kind == SessionKind::Topic
                && s.superseded_by.is_none()
        }) {
            let running = self
                .manager
                .live(&s.id)
                .await
                .is_some_and(|h| h.is_running());
            candidates.push(Candidate {
                topic: s.topic.or(s.title).unwrap_or_default(),
                running,
                focus: focus.as_deref() == Some(s.id.as_str()),
                id: s.id,
            });
        }
        Ok(candidates)
    }

    async fn create(
        &self,
        binding: &str,
        kind: SessionKind,
        routed_by: &str,
    ) -> Result<Arc<ManagedSession>, String> {
        let origin = self.origin(binding)?;
        self.manager
            .create_session_with(
                self.manager.workspace().to_path_buf(),
                self.model(),
                None,
                None,
                agent_toggles(),
                |header| {
                    header.kind = kind;
                    header.origin = Some(origin);
                    header.routed_by = Some(routed_by.to_string());
                },
            )
            .await
    }

    // ================== 命令 ==================

    async fn command(self: &Arc<Self>, msg: &Inbound, cmd: Command) -> Result<(), String> {
        let binding = &msg.binding;
        let reply = match cmd {
            Command::New(text) if text.is_empty() => return Err("用法: /new <消息>".into()),
            Command::New(text) => {
                let files = self.state.lock().await.pending.take(binding);
                return self.route_and_run(msg, text, files, true).await;
            }
            Command::Switch(prefix) => self.switch(binding, &prefix).await?,
            Command::Sessions => self.list_sessions(binding).await?,
            Command::Stop => {
                self.focus_handle(binding).await?.abort_current().await?;
                "已请求中止当前轮次".to_string()
            }
            Command::Status => self.status(binding).await?,
            Command::Delete => self.delete_focus(binding).await?,
        };
        self.say(binding, Some(&msg.message_id), reply).await;
        Ok(())
    }

    async fn focus_handle(&self, binding: &str) -> Result<Arc<ManagedSession>, String> {
        let focus = self
            .state
            .lock()
            .await
            .bindings
            .focus(binding)
            .map(str::to_string);
        let id = focus.ok_or("当前没有默认会话，可直接发消息或 /new <消息> 新建")?;
        self.manager.live_or_load(&id).await
    }

    async fn switch(&self, binding: &str, prefix: &str) -> Result<String, String> {
        let candidates = self.candidates(binding).await?;
        let id = resolve_prefix(&candidates, prefix).map_err(|e| e.to_string())?;
        self.state
            .lock()
            .await
            .bindings
            .set_focus(binding, Some(id.clone()))?;
        Ok(format!("已切换到 {id}"))
    }

    async fn list_sessions(&self, binding: &str) -> Result<String, String> {
        let mut candidates = self.candidates(binding).await?;
        if candidates.is_empty() {
            return Ok("暂无会话".into());
        }
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(candidates
            .iter()
            .map(|c| {
                format!(
                    "{}{} {} {}",
                    if c.focus { "* " } else { "  " },
                    c.id,
                    if c.running { "[运行中]" } else { "" },
                    c.topic
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn delete_focus(&self, binding: &str) -> Result<String, String> {
        let handle = self.focus_handle(binding).await?;
        self.manager.delete_session(&handle).await?;
        self.state
            .lock()
            .await
            .bindings
            .replace_focus(&handle.id, None)?;
        Ok(format!("已删除会话 {}", handle.id))
    }

    async fn status(&self, binding: &str) -> Result<String, String> {
        let pending = self.state.lock().await.pending.count(binding);
        let focus = self
            .state
            .lock()
            .await
            .bindings
            .focus(binding)
            .map(str::to_string);
        let Some(id) = focus else {
            return Ok(format!("默认会话: 无\n待附件: {pending}"));
        };
        let handle = self.manager.live_or_load(&id).await?;
        let session = handle.snapshot().session.clone();
        Ok(format!(
            "默认会话: {id}\n运行中: {}\n模型: {}\n上下文: {}\ncache 命中率: {}\n待附件: {pending}",
            if handle.is_running() { "是" } else { "否" },
            session.header.current_model,
            self.context_usage(&session),
            cache_hit_rate(&session)
                .map_or("暂无数据".into(), |r| format!("{:.0}%", r * 100.0)),
        ))
    }

    fn context_usage(&self, session: &Session) -> String {
        let Some(tokens) = context_tokens(session) else {
            return "暂无数据".into();
        };
        let limit = self
            .manager
            .app_config()
            .resolve_model(
                &session.header.current_model,
                session.header.current_variant.as_deref(),
            )
            .ok()
            .and_then(|m| m.model_config.limit.as_ref().map(|l| l.context as u64));
        match limit {
            Some(limit) => format!(
                "{tokens} / {limit} ({:.0}%)",
                tokens as f64 * 100.0 / limit as f64
            ),
            None => format!("{tokens} (模型未声明 limit.context)"),
        }
    }

    // ================== 轮次与回复 ==================

    /// 投递并等待一轮，回复最终文本；随后检查 rollover
    async fn run_turn(
        self: Arc<Self>,
        handle: Arc<ManagedSession>,
        binding: String,
        reply_to: Option<String>,
        prompt: String,
    ) {
        let text = match self.deliver(&handle, &binding, &prompt).await {
            Ok(text) if text.trim().is_empty() => "（本轮没有文本输出）".to_string(),
            Ok(text) => text,
            Err(error) => format!("本轮执行失败: {error}"),
        };
        let ids = self.say(&binding, reply_to.as_deref(), text).await;
        {
            let mut state = self.state.lock().await;
            for id in ids {
                if let Err(e) = state.index.record(&self.key(&id), &handle.id) {
                    tracing::warn!("记录出站消息索引失败: {e}");
                }
            }
        }
        self.after_turn(&handle, &binding).await;
    }

    /// `deliver_wait` 并守卫 AskUser：渠道暂不支持提问，出现即中止本轮，不挂起
    async fn deliver(
        &self,
        handle: &Arc<ManagedSession>,
        binding: &str,
        prompt: &str,
    ) -> Result<String, String> {
        let mut view = handle.subscribe();
        let turn = handle.deliver_wait(prompt);
        tokio::pin!(turn);
        let mut asked = false;
        let outcome = loop {
            tokio::select! {
                outcome = &mut turn => break outcome,
                changed = view.changed(), if !asked => {
                    if changed.is_ok() && view.borrow().pending_user_question.is_some() {
                        asked = true;
                        self.say(binding, None, "当前渠道暂不支持提问，本轮已中止").await;
                        if let Err(e) = handle.abort_current().await {
                            tracing::warn!("中止提问中的会话失败: {e}");
                        }
                    }
                }
            }
        };
        if asked {
            return Err("当前渠道暂不支持提问（AskUser）".into());
        }
        outcome.map(|report| report.text)
    }

    async fn after_turn(&self, handle: &Arc<ManagedSession>, binding: &str) {
        match maybe_rollover(&self.manager, &self.config, handle).await {
            Ok(None) => {}
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
                self.say(
                    binding,
                    None,
                    format!("上下文接近上限，已压缩并切换到新会话 {}", rolled.new_id),
                )
                .await;
            }
            Err(error) => {
                self.say(
                    binding,
                    None,
                    format!("上下文压缩失败，下个回合结束重试: {error}"),
                )
                .await;
            }
        }
    }

    // ================== 出站与推送 ==================

    /// 发送文本，返回渠道消息 id；发送失败只记日志
    async fn say(
        &self,
        binding: &str,
        reply_to: Option<&str>,
        text: impl Into<String>,
    ) -> Vec<String> {
        let msg = Outbound::Text(OutboundText {
            binding: binding.to_string(),
            text: text.into(),
            reply_to: reply_to.map(str::to_string),
        });
        self.channel.send(msg).await.unwrap_or_else(|e| {
            tracing::warn!("发送到 {binding} 失败: {e}");
            Vec::new()
        })
    }

    /// Lifecycle 整理完成回调：有更新或失败时推送到来源 binding
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
        match report.origin {
            Some(origin) => {
                self.say(
                    &format!("{}:{}", origin.channel, origin.chat_id),
                    None,
                    text,
                )
                .await;
            }
            None => tracing::warn!("curator 报告无 origin，未推送: {text}"),
        }
    }

    /// 待附件超时：提示用户并清理缓冲（文件仍保留在磁盘）
    async fn expire_pending(&self) {
        let ttl = chrono::Duration::seconds(self.config.pending_files_ttl_secs as i64);
        let expired = self.state.lock().await.pending.expire(Utc::now(), ttl);
        for (binding, files) in expired {
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

    // ================== 定时任务 ==================

    pub async fn fire(self: &Arc<Self>, task: Task) {
        if let Err(error) = self.try_fire(&task).await {
            tracing::warn!("定时任务 {} 已跳过: {error}", task.id);
        }
    }

    async fn try_fire(self: &Arc<Self>, task: &Task) -> Result<(), String> {
        let (handle, binding) = match &task.target {
            Target::Session(id) => {
                let handle = self
                    .manager
                    .live_or_load(id)
                    .await
                    .map_err(|e| format!("目标 session {id} 不可用: {e}"))?;
                let binding = Self::origin_binding(&handle, id)?;
                (handle, binding)
            }
            Target::Binding(binding) => (
                self.create(binding, SessionKind::Oneshot, "scheduled")
                    .await?,
                binding.clone(),
            ),
        };
        let prompt = format_scheduled_reminder(&task.id, &task.expr, &task.prompt);
        tokio::spawn(self.clone().run_turn(handle, binding, None, prompt));
        Ok(())
    }

    fn origin_binding(handle: &Arc<ManagedSession>, id: &str) -> Result<String, String> {
        let session = handle.snapshot().session.clone();
        let origin = session
            .header
            .origin
            .as_ref()
            .ok_or_else(|| format!("session {id} 无 origin，无法回复"))?;
        Ok(format!("{}:{}", origin.channel, origin.chat_id))
    }

    /// 停机期间错过的任务：按 binding 汇总提示一次，不补跑
    async fn report_missed(&self, missed: Vec<Missed>) {
        let mut by_binding: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for m in missed {
            let binding = match &m.task.target {
                Target::Binding(b) => Ok(b.clone()),
                Target::Session(id) => match self.manager.live_or_load(id).await {
                    Ok(handle) => Self::origin_binding(&handle, id),
                    Err(e) => Err(format!("目标 session {id} 不可用: {e}")),
                },
            };
            match binding {
                Ok(b) => by_binding.entry(b).or_default().push(format!(
                    "{}（最近应于 {}）",
                    m.task.id,
                    m.last.format("%Y-%m-%d %H:%M")
                )),
                Err(e) => tracing::warn!("错过的定时任务 {} 无法通知: {e}", m.task.id),
            }
        }
        for (binding, lines) in by_binding {
            self.say(
                &binding,
                None,
                format!("停机期间错过了以下定时任务，未补跑: {}", lines.join("；")),
            )
            .await;
        }
    }

    // ================== 标识 ==================

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

/// 用户消息 + 被引用上下文 + 附件绝对路径（历史里只写路径文本）
fn compose_prompt(text: &str, files: &[AttachedFile], quoted: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(quoted) = quoted {
        out.push_str(&format!("[引用的消息]\n{quoted}\n\n"));
    }
    out.push_str(text);
    if !files.is_empty() {
        out.push_str("\n\n[附件]");
        for file in files {
            out.push_str(&format!("\n{}", file.path.display()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_space::channel::{self, FileSource, InboundFile};
    use crate::agent_space::router::{ModelReply, RouteModel};
    use crate::config::model::ModelLimitConfig;
    use crate::config::{ModelConfig, ProviderConfig, ProviderType};
    use crate::storage::jsonl::JsonlSessionStore;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn prompt_carries_quote_and_attachment_paths_in_order() {
        let file = |p: &str| AttachedFile {
            path: PathBuf::from(p),
            name: "x".into(),
            kind: FileKind::File,
        };
        let out = compose_prompt(
            "看看",
            &[file("/a/1.pdf"), file("/a/2.png")],
            Some("旧消息"),
        );
        assert_eq!(
            out,
            "[引用的消息]\n旧消息\n\n看看\n\n[附件]\n/a/1.pdf\n/a/2.png"
        );
        assert_eq!(compose_prompt("hi", &[], None), "hi");
    }

    /// 内存假 channel：记录出站文本，id 依次为 out1、out2…
    #[derive(Default)]
    struct FakeChannel {
        sent: StdMutex<Vec<OutboundText>>,
    }

    #[async_trait::async_trait]
    impl Channel for FakeChannel {
        fn name(&self) -> &'static str {
            "fake"
        }
        async fn send(&self, msg: Outbound) -> channel::Result<Vec<String>> {
            let Outbound::Text(text) = msg else {
                panic!("本期只发文本");
            };
            let mut sent = self.sent.lock().unwrap();
            sent.push(text);
            Ok(vec![format!("out{}", sent.len())])
        }
        async fn run(self: Arc<Self>, _inbound: mpsc::Sender<Inbound>) -> channel::Result<()> {
            Ok(())
        }
    }

    impl FakeChannel {
        /// 等待累计出站消息达到 `n` 条
        async fn wait(&self, n: usize) -> Vec<OutboundText> {
            for _ in 0..200 {
                {
                    let sent = self.sent.lock().unwrap();
                    if sent.len() >= n {
                        return sent.clone();
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("等待 {n} 条出站消息超时: {:?}", self.sent.lock().unwrap());
        }
    }

    /// 任何调用都视为失败：L0 路径不应触达小模型
    struct NoModel;

    #[async_trait::async_trait]
    impl RouteModel for NoModel {
        async fn decide(&self, _: &str, _: &str) -> Result<ModelReply, String> {
            Err("L0 路径不应调用小模型".into())
        }
    }

    #[derive(Debug)]
    struct BytesSource(&'static [u8]);

    #[async_trait::async_trait]
    impl FileSource for BytesSource {
        async fn download(&self, dest: &Path) -> channel::Result<()> {
            tokio::fs::write(dest, self.0).await?;
            Ok(())
        }
    }

    /// 按 Anthropic Messages 流式规范应答固定文本的本地端点
    async fn mock_anthropic() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut conn, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = conn.read(&mut chunk).await.unwrap_or(0);
                        buf.extend_from_slice(&chunk[..n]);
                        if n == 0 || request_complete(&buf) {
                            break;
                        }
                    }
                    let events = [
                        r#"{"type":"message_start","message":{"usage":{"input_tokens":10}}}"#,
                        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"pong"}}"#,
                        r#"{"type":"content_block_stop","index":0}"#,
                        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
                        r#"{"type":"message_stop"}"#,
                    ];
                    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                    let _ = conn.write_all(format!("{head}{body}").as_bytes()).await;
                });
            }
        });
        url
    }

    fn request_complete(buf: &[u8]) -> bool {
        let text = String::from_utf8_lossy(buf);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let length = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        body.len() >= length
    }

    struct Fixture {
        runtime: Arc<Runtime>,
        channel: Arc<FakeChannel>,
        manager: Arc<SessionManager>,
        dir: PathBuf,
    }

    async fn fixture() -> Fixture {
        let dir = std::env::temp_dir().join(format!("vva-runtime-{}", uuid::Uuid::new_v4()));
        let mut app = crate::config::AppConfig::default();
        app.settings.default = "p/m".into();
        app.providers.insert(
            "p".into(),
            ProviderConfig {
                api_key: "k".into(),
                base_url: mock_anthropic().await,
                provider_type: Some(ProviderType::Anthropic),
                models: [(
                    "m".to_string(),
                    ModelConfig {
                        name: "M".into(),
                        limit: Some(ModelLimitConfig {
                            context: 1_000_000,
                            output: 1000,
                        }),
                        ..Default::default()
                    },
                )]
                .into(),
            },
        );
        let manager = Arc::new(SessionManager::new(
            Arc::new(JsonlSessionStore::new(dir.join("store")).unwrap()),
            app,
            &dir,
            dir.join("state.json"),
        ));
        let channel = Arc::new(FakeChannel::default());
        let router = Router::new(Box::new(NoModel), dir.join("router.jsonl"));
        let runtime = Runtime::new(
            manager.clone(),
            AgentSpaceConfig::default(),
            channel.clone(),
            router,
        )
        .unwrap();
        Fixture {
            runtime,
            channel,
            manager,
            dir,
        }
    }

    const BINDING: &str = "fake:c2c:u1";

    fn inbound(id: &str, text: &str) -> Inbound {
        Inbound {
            binding: BINDING.into(),
            user_id: "u1".into(),
            message_id: id.into(),
            text: text.into(),
            files: Vec::new(),
            reply_to: None,
        }
    }

    impl Fixture {
        async fn session_ids(&self) -> Vec<String> {
            let mut ids: Vec<_> = self
                .manager
                .store()
                .list_sessions()
                .await
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect();
            ids.sort();
            ids
        }

        async fn focus(&self) -> Option<String> {
            self.runtime
                .state
                .lock()
                .await
                .bindings
                .focus(BINDING)
                .map(str::to_string)
        }
    }

    #[tokio::test]
    async fn first_message_creates_session_replies_and_reply_anchor_skips_router() {
        let f = fixture().await;
        f.runtime.handle(inbound("m1", "hello")).await;
        let sent = f.channel.wait(1).await;
        assert_eq!(sent[0].text, "pong");
        assert_eq!(sent[0].reply_to.as_deref(), Some("m1"));

        let ids = f.session_ids().await;
        assert_eq!(ids.len(), 1);
        assert_eq!(f.focus().await.as_deref(), Some(ids[0].as_str()));
        let header = f
            .manager
            .live(&ids[0])
            .await
            .unwrap()
            .snapshot()
            .session
            .header
            .clone();
        assert_eq!(header.origin.unwrap().chat_id, "c2c:u1");
        assert_eq!(header.routed_by.as_deref(), Some("first"));
        assert!(!header.team);

        // 回复 bot 出站消息：L0 直达同一 session（NoModel 保证未调用小模型）
        let mut follow = inbound("m2", "继续");
        follow.reply_to = Some(channel::ReplyRef {
            message_id: "out1".into(),
            text: "pong".into(),
        });
        f.runtime.handle(follow).await;
        let sent = f.channel.wait(2).await;
        assert_eq!(sent[1].text, "pong");
        assert_eq!(f.session_ids().await, ids);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn files_without_text_are_buffered_then_merged_into_next_message() {
        let f = fixture().await;
        let mut only_file = inbound("m1", "");
        only_file.files = vec![InboundFile::new(
            "a.pdf".into(),
            "application/pdf".into(),
            3,
            BytesSource(b"pdf"),
        )];
        f.runtime.handle(only_file).await;
        let sent = f.channel.wait(1).await;
        assert!(sent[0].text.contains("已收到"), "{}", sent[0].text);
        assert!(f.session_ids().await.is_empty(), "无文字文件不触发路由");

        f.runtime.handle(inbound("m2", "总结一下")).await;
        f.channel.wait(2).await;
        let id = f.session_ids().await.remove(0);
        let session = f
            .manager
            .live(&id)
            .await
            .unwrap()
            .snapshot()
            .session
            .clone();
        let user = session
            .turns
            .iter()
            .find(|t| t.text_content().contains("总结一下"))
            .unwrap();
        let path = f.dir.join("files/fake_c2c_u1/a.pdf");
        assert!(user.text_content().contains(&path.display().to_string()));
        assert_eq!(std::fs::read(&path).unwrap(), b"pdf");
        assert_eq!(f.runtime.state.lock().await.pending.count(BINDING), 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn switch_resolves_prefix_and_rejects_ambiguity() {
        let f = fixture().await;
        f.runtime.handle(inbound("m1", "/new 第一个")).await;
        f.channel.wait(1).await;
        f.runtime.handle(inbound("m2", "/new 第二个")).await;
        f.channel.wait(2).await;
        let ids = f.session_ids().await;
        assert_eq!(ids.len(), 2);
        let focus = f.focus().await.unwrap();
        let other = ids.iter().find(|id| **id != focus).unwrap().clone();
        f.runtime
            .handle(inbound("m3", &format!("/switch {other}")))
            .await;
        let sent = f.channel.wait(3).await;
        assert!(sent[2].text.contains(&other), "{}", sent[2].text);
        assert_eq!(f.focus().await, Some(other));

        f.runtime.handle(inbound("m4", "/switch sess_")).await;
        let sent = f.channel.wait(4).await;
        assert!(sent[3].text.contains("匹配多个"), "{}", sent[3].text);

        f.runtime.handle(inbound("m5", "/sessions")).await;
        let sent = f.channel.wait(5).await;
        assert!(ids.iter().all(|id| sent[4].text.contains(id.as_str())));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn scheduled_task_runs_in_oneshot_session_and_replies_to_binding() {
        let f = fixture().await;
        let task: Task = serde_json::from_str(
            r#"{"id":"water","cron":"* * * * *","prompt":"喝水","binding":"fake:c2c:u1"}"#,
        )
        .unwrap();
        f.runtime.fire(task).await;
        let sent = f.channel.wait(1).await;
        assert_eq!(sent[0].text, "pong");
        assert_eq!(sent[0].binding, BINDING);
        let id = f.session_ids().await.remove(0);
        let session = f
            .manager
            .live(&id)
            .await
            .unwrap()
            .snapshot()
            .session
            .clone();
        assert_eq!(session.header.kind, SessionKind::Oneshot);
        assert!(
            session
                .turns
                .iter()
                .any(|t| t.text_content().contains("type=\"scheduled\""))
        );
        assert_eq!(f.focus().await, None, "oneshot 不抢占 focus");
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}
