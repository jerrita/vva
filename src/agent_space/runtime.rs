//! Daemon 运行时：入站消息 → 命令 / 文件缓冲 → 路由 → session 轮次 → 出站文本。
//!
//! 路由与会话创建在入站循环内顺序完成（保证同一 binding 的决策有序），
//! 轮次执行与回复在独立任务中进行，因此运行中的 session 仍可接收插话。
//! 本期只回最终文本，不做流式增量发送。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

use super::agent_toggles;
use super::binding::{AttachedFile, BindingStore, FileKind, MessageIndex, PendingFiles};
use super::channel::files::{sanitize_filename, unique_dest};
use super::channel::{Channel, Inbound, Outbound, OutboundText};
use super::command::{self, Command};
use super::curator::{Curator, CuratorReport, CuratorStatus, ModelSource};
use super::progress;
use super::rollover::{cache_hit_rate, context_tokens, maybe_rollover};
use super::router::{Candidate, ReplyRef, Route, RouteInput, Routed, Router, resolve_prefix};
use super::scheduler::{self, Failure, Missed, ScheduleStore, Task, Trigger, describe_task};
use crate::config::AgentSpaceConfig;
use crate::harness::{ManagedSession, SessionManager};
use crate::model::{Origin, Session, SessionKind, ToggleOverrides};
use crate::system::format_scheduled_reminder;
use crate::ui::history::compute_stats;

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

/// agent 模式的模型选择，持久化于 `<workspace>/model.json`，优先于配置文件
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModeModel {
    model: String,
    #[serde(default)]
    variant: Option<String>,
}

const MODE_MODEL_FILE: &str = "model.json";

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
        let mode_model = load_mode_model(&config, &manager)?;
        let mode_model = Arc::new(std::sync::Mutex::new(mode_model));
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

    /// topic 与 oneshot session 使用的模型规格
    pub fn model(&self) -> String {
        self.mode_model.lock().expect("mode_model").model.clone()
    }

    fn mode_model(&self) -> ModeModel {
        self.mode_model.lock().expect("mode_model").clone()
    }

    /// 校验并设置 agent 模式模型（需声明 `limit.context`，与启动期校验一致）并落盘
    fn set_mode_model(&self, model: &str, variant: Option<String>) -> Result<(), String> {
        let next = check_mode_model(&self.manager, model, variant)?;
        let raw = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        let path = self.manager.workspace().join(MODE_MODEL_FILE);
        std::fs::write(&path, raw).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
        *self.mode_model.lock().expect("mode_model") = next;
        Ok(())
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

    // ================== 入站 ==================

    /// 处理一条入站消息；失败原样回复用户
    pub async fn handle(self: &Arc<Self>, msg: Inbound) {
        tracing::info!(
            "收到入站: binding={} message_id={} 文字 {} 字符, 附件 {}",
            msg.binding,
            msg.message_id,
            msg.text.chars().count(),
            msg.files.len()
        );
        tracing::debug!("入站正文预览: {}", preview(&msg.text));
        if let Err(error) = self.dispatch(&msg).await {
            tracing::warn!("入站处理失败 binding={}: {error}", msg.binding);
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
            tracing::debug!("缓冲 {} 个附件: binding={}", files.len(), msg.binding);
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
        tracing::info!(
            "路由结果: binding={} {} routed_by={} warning={}",
            msg.binding,
            describe_route(&routed.route),
            routed.routed_by,
            routed.warning.is_some()
        );
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
                title: s.title.unwrap_or_default(),
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
        let mode = self.mode_model();
        let created = self
            .manager
            .create_session_with(
                self.manager.workspace().to_path_buf(),
                mode.model,
                mode.variant,
                None,
                agent_toggles(),
                |header| {
                    header.kind = kind;
                    header.origin = Some(origin);
                    header.routed_by = Some(routed_by.to_string());
                },
            )
            .await?;
        tracing::info!(
            "新建会话: id={} kind={kind:?} model={} binding={binding} routed_by={routed_by}",
            created.id,
            self.model()
        );
        Ok(created)
    }

    // ================== 命令 ==================

    async fn command(self: &Arc<Self>, msg: &Inbound, cmd: Command) -> Result<(), String> {
        let binding = &msg.binding;
        tracing::info!("处理命令: {} binding={binding}", command_name(&cmd));
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
            Command::Rename(title) => self.rename(binding, &title).await?,
            Command::Model(arg) => self.model_command(binding, &arg).await?,
            Command::Skills => self.skills(),
            Command::Schedule(arg) => self.schedule(&arg).await?,
            Command::Delete(prefix) => self.delete(binding, &prefix).await?,
            Command::Clear(prefix) => {
                let handle = self.target_handle(binding, &prefix).await?;
                self.remove(&handle).await?;
                format!("已删除会话 {}", handle.id)
            }
            Command::Help => command::help(),
            Command::Unknown(name) => {
                return Err(format!("未知命令 /{name}\n{}", command::help()));
            }
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
                // 行首空白会被 IM 渠道裁掉，用可见标记保持对齐
                let running = if c.running { " [运行中]" } else { "" };
                format!(
                    "{} {}{} {}",
                    if c.focus { "●" } else { "○" },
                    c.id,
                    running,
                    c.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// 空前缀取默认会话，否则按前缀解析
    async fn target_handle(
        &self,
        binding: &str,
        prefix: &str,
    ) -> Result<Arc<ManagedSession>, String> {
        if prefix.is_empty() {
            return self.focus_handle(binding).await;
        }
        let candidates = self.candidates(binding).await?;
        let id = resolve_prefix(&candidates, prefix).map_err(|e| e.to_string())?;
        self.manager.live_or_load(&id).await
    }

    /// 删除会话并清理游标与 focus（不经 curator）
    async fn remove(&self, handle: &Arc<ManagedSession>) -> Result<(), String> {
        self.manager.delete_session(handle).await?;
        self.curator.forget(&handle.id).await?;
        self.state
            .lock()
            .await
            .bindings
            .replace_focus(&handle.id, None)
    }

    /// 整理记忆后删除：整理耗时较长，后台执行并回报；整理失败则保留会话
    async fn delete(self: &Arc<Self>, binding: &str, prefix: &str) -> Result<String, String> {
        let handle = self.target_handle(binding, prefix).await?;
        if handle.is_running() {
            return Err("会话正在运行，请先 /stop，或用 /clear 直接删除".into());
        }
        let id = handle.id.clone();
        let (this, binding) = (self.clone(), binding.to_string());
        tokio::spawn({
            let id = id.clone();
            async move {
                let reply = match this.curate_then_remove(&handle).await {
                    Ok(()) => format!("已整理并删除会话 {id}"),
                    Err(error) => format!("删除失败，会话 {id} 已保留: {error}"),
                };
                this.say(&binding, None, reply).await;
            }
        });
        Ok(format!("正在整理记忆，完成后删除会话 {id}"))
    }

    async fn curate_then_remove(&self, handle: &Arc<ManagedSession>) -> Result<(), String> {
        let seen_turns = handle.turn_seq();
        let result = self.curator.curate(&handle.id).await;
        let failed = (result.status == CuratorStatus::Failed).then(|| result.summary.clone());
        let origin = handle.snapshot().session.header.origin.clone();
        self.push_curator(CuratorReport {
            session_id: handle.id.clone(),
            origin,
            result,
        })
        .await;
        if let Some(summary) = failed {
            return Err(summary);
        }
        if handle.is_running() || handle.turn_seq() != seen_turns {
            return Err("整理期间会话又有活动".into());
        }
        self.remove(handle).await
    }

    async fn rename(&self, binding: &str, title: &str) -> Result<String, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("用法: /rename <新标题>".into());
        }
        let handle = self.focus_handle(binding).await?;
        handle.rename(title).await?;
        Ok(format!("已重命名 {} 为: {title}", handle.id))
    }

    /// 无参查看；`list` 列出可用规格；`<规格> [档位]` 切换 focus 会话模型，
    /// 无 focus 时改 agent 模式模型；`--global` 同时改 agent 模式与 focus 会话
    async fn model_command(&self, binding: &str, arg: &str) -> Result<String, String> {
        let mut global = false;
        let mut flag_variant: Option<&str> = None;
        let mut words: Vec<&str> = Vec::new();
        let mut tokens = arg.split_whitespace();
        while let Some(w) = tokens.next() {
            match w {
                "--global" => global = true,
                "--variant" => {
                    flag_variant = Some(tokens.next().ok_or("--variant 需要指定档位")?);
                }
                _ => words.push(w),
            }
        }
        let focus = self.focus_handle(binding).await.ok();
        match words[..] {
            [] if flag_variant.is_none() && global => {
                Err("用法: /model --global <规格> [档位]".into())
            }
            [] if flag_variant.is_none() => {
                let mode = self.mode_model();
                let mut lines = Vec::new();
                if let Some(handle) = &focus {
                    let header = handle.snapshot().session.header.clone();
                    lines.push(format!(
                        "默认会话 {} 模型: {}\n  当前档位: {}\n  可用档位: {}",
                        handle.id,
                        header.current_model,
                        header.current_variant.as_deref().unwrap_or("none"),
                        self.variants_label(&header.current_model)
                    ));
                }
                lines.push(format!(
                    "agent 模式模型（新会话）: {}\n  当前档位: {}\n  可用档位: {}",
                    mode.model,
                    mode.variant.as_deref().unwrap_or("none"),
                    self.variants_label(&mode.model)
                ));
                Ok(lines.join("\n"))
            }
            ["list"] => {
                let session = focus
                    .as_ref()
                    .map(|h| h.snapshot().session.header.current_model.clone());
                let mode = self.model();
                let config = self.manager.app_config();
                let mut lines: Vec<(String, String)> = config
                    .providers
                    .iter()
                    .flat_map(|(provider, p)| {
                        p.models.iter().map(move |(id, m)| {
                            let spec = format!("{provider}/{id}");
                            let mut info = vec![m.name.clone()];
                            match &m.limit {
                                Some(l) => info.push(format!("上下文 {}k", l.context / 1000)),
                                None => info.push("未声明 limit.context（不可选用）".into()),
                            }
                            if let Some(v) = m.variants.as_ref().filter(|v| !v.is_empty()) {
                                info.push(format!("档位 {}", v.join("/")));
                            }
                            (spec, info.join("，"))
                        })
                    })
                    .collect();
                lines.sort();
                Ok(lines
                    .iter()
                    .map(|(spec, info)| {
                        let mut mark = String::new();
                        if session.as_deref() == Some(spec) {
                            mark.push_str(" [会话]");
                        }
                        if *spec == mode {
                            mark.push_str(" [模式]");
                        }
                        format!("{spec}{mark}  {info}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            _ if flag_variant.is_some() && words.len() <= 1 => {
                self.apply_model(focus, global, words.first().copied(), flag_variant)
                    .await
            }
            [spec] | [spec, _] if !spec.is_empty() => {
                self.apply_model(focus, global, Some(spec), words.get(1).copied())
                    .await
            }
            _ => Err("用法: /model [list|[--global] [<规格>] [--variant <档位>]]".into()),
        }
    }

    /// 模型声明的可用档位（`/` 分隔），未声明时给出提示
    fn variants_label(&self, spec: &str) -> String {
        self.manager
            .app_config()
            .resolve_model(spec, None)
            .ok()
            .and_then(|m| m.model_config.variants.clone())
            .filter(|v| !v.is_empty())
            .map_or("无".into(), |v| v.join("/"))
    }

    /// 切换模型与/或档位：`spec` 缺省时沿用当前模型；档位 `none` 清除
    async fn apply_model(
        &self,
        focus: Option<Arc<ManagedSession>>,
        global: bool,
        spec: Option<&str>,
        variant: Option<&str>,
    ) -> Result<String, String> {
        {
            {
                let spec_owned;
                let spec = match spec {
                    Some(spec) => spec,
                    None => {
                        spec_owned = match &focus {
                            Some(h) if !global => h.snapshot().session.header.current_model.clone(),
                            _ => self.mode_model().model,
                        };
                        spec_owned.as_str()
                    }
                };
                let variant = variant.filter(|v| *v != "none").map(str::to_string);
                let mode = global || focus.is_none();
                // 会话与模式都要求声明 limit.context，否则 rollover 无法判定
                check_mode_model(&self.manager, spec, variant.clone())?;
                let mut lines = Vec::new();
                if let Some(handle) = focus {
                    handle
                        .configure(
                            Some(spec.into()),
                            Some(variant.clone()),
                            ToggleOverrides::default(),
                        )
                        .await?;
                    lines.push(format!(
                        "默认会话 {} 已切换到 {}（下一轮起生效，切换会使该会话缓存失效）",
                        handle.id,
                        spec_label(spec, variant.as_deref())
                    ));
                }
                if mode {
                    self.set_mode_model(spec, variant.clone())?;
                    lines.push(format!(
                        "agent 模式模型已设为 {}（新会话使用）",
                        spec_label(spec, variant.as_deref())
                    ));
                }
                Ok(lines.join("\n"))
            }
        }
    }

    fn skills(&self) -> String {
        let skills = crate::skills::discover_skills(self.manager.workspace());
        if skills.is_empty() {
            return "暂无技能".into();
        }
        let mut rows = vec![
            "| 名称 | 来源 | 描述 |".to_string(),
            "|---|---|---|".to_string(),
        ];
        rows.extend(skills.iter().map(|s| {
            let source = match s.source {
                crate::skills::SkillSource::Global => "全局",
                crate::skills::SkillSource::Project => "项目",
            };
            format!(
                "| {} | {source} | {} |",
                s.name,
                skill_desc_cell(&s.description)
            )
        }));
        rows.join("\n")
    }

    /// `/schedule [list]`、`pause|resume|delete|run <id 前缀>`；创建走 Schedule 工具（自然语言）
    async fn schedule(self: &Arc<Self>, arg: &str) -> Result<String, String> {
        let usage = "用法: /schedule [list] | pause|resume|delete|run <id>（创建请直接告诉 agent）";
        let mut words = arg.split_whitespace();
        let action = words.next().unwrap_or("list");
        let id = words.next();
        if words.next().is_some() {
            return Err(usage.into());
        }
        match (action, id) {
            ("list", None) => {
                self.schedule.refresh();
                let tasks = self.schedule.list();
                if tasks.is_empty() {
                    return Ok("暂无定时任务".into());
                }
                Ok(tasks
                    .iter()
                    .map(|(t, s)| describe_task(t, s))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            ("pause", Some(id)) => Ok(format!(
                "已暂停 {}",
                self.schedule.set_enabled(id, false)?.id
            )),
            ("resume", Some(id)) => Ok(format!(
                "已恢复 {}",
                self.schedule.set_enabled(id, true)?.id
            )),
            ("delete", Some(id)) => Ok(format!("已删除 {}", self.schedule.remove(id)?.id)),
            ("run", Some(id)) => {
                let task = self.schedule.get(id)?;
                let enabled = self
                    .schedule
                    .list()
                    .into_iter()
                    .any(|(t, s)| t.id == task.id && s.enabled);
                if !enabled {
                    return Err(format!("任务 {} 已停用，请先 /schedule resume", task.id));
                }
                if self.fire(task.clone()).await {
                    Ok(format!("已触发 {}", task.id))
                } else {
                    Err(format!("任务 {} 正在执行", task.id))
                }
            }
            _ => Err(usage.into()),
        }
    }

    async fn status(&self, binding: &str) -> Result<String, String> {
        let (pending, focus) = {
            let state = self.state.lock().await;
            (
                state.pending.count(binding),
                state.bindings.focus(binding).map(str::to_string),
            )
        };
        let Some(id) = focus else {
            return Ok(status_table(&[
                ("默认会话", "无".into()),
                ("待附件", pending.to_string()),
            ]));
        };
        let handle = self.manager.live_or_load(&id).await?;
        let session = handle.snapshot().session.clone();
        let stats = compute_stats(&session, &self.manager.app_config());
        let header = &session.header;
        let (provider, model) = header
            .current_model
            .split_once('/')
            .unwrap_or(("-", &header.current_model));
        let context = match (context_tokens(&session), stats.context_limit) {
            (None, _) => "暂无数据".into(),
            (Some(t), Some(limit)) => {
                format!("{t} / {limit} ({:.0}%)", t as f64 * 100.0 / limit as f64)
            }
            (Some(t), None) => format!("{t} (模型未声明 limit.context)"),
        };
        let rows = [
            ("默认会话", id),
            ("标题", header.title.clone().unwrap_or_else(|| "-".into())),
            ("工作区", header.workspace_root.display().to_string()),
            (
                "运行中",
                if handle.is_running() { "是" } else { "否" }.into(),
            ),
            ("轮次", stats.total_turns.to_string()),
            ("模型", model.to_string()),
            ("Provider", provider.to_string()),
            (
                "推理档位",
                header
                    .current_variant
                    .clone()
                    .unwrap_or_else(|| "none".into()),
            ),
            ("上下文", context),
            (
                "输入 / 输出",
                format!("{} / {}", stats.prompt_tokens, stats.completion_tokens),
            ),
            (
                "TPS",
                stats.tps.map_or("暂无数据".into(), |v| format!("{v:.1}")),
            ),
            ("Cache 读", stats.cache_read_tokens.to_string()),
            ("Cache 写", stats.cache_write_tokens.to_string()),
            (
                "Cache 命中率",
                cache_hit_rate(&session)
                    .map_or("暂无数据".into(), |r| format!("{:.0}%", r * 100.0)),
            ),
            ("预估费用", format!("${:.4}", stats.estimated_cost_usd)),
            ("待附件", pending.to_string()),
        ];
        Ok(status_table(&rows))
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
        tracing::info!(
            "Deliver 开始: session={} binding={binding} prompt {} 字符",
            handle.id,
            prompt.chars().count()
        );
        let started = std::time::Instant::now();
        let turns_before = handle.snapshot().session.turns.len();
        let delivered = self
            .deliver_reporting(&handle, std::slice::from_ref(&binding), &prompt)
            .await;
        let usage = turn_usage(&handle.snapshot().session, turns_before);
        tracing::info!(
            "Deliver 结束: session={} 轮次={} 耗时 {:.1}s prompt/completion/cached tokens={}/{}/{} 出错={}",
            handle.id,
            delivered.as_ref().map_or(0, |r| r.turn_seq),
            started.elapsed().as_secs_f32(),
            usage.0,
            usage.1,
            usage.2,
            delivered.is_err()
        );
        let text = match delivered.map(|report| report.text) {
            Ok(text) if text.trim().is_empty() => "（本轮没有文本输出）".to_string(),
            Ok(text) => text,
            Err(error) => {
                tracing::warn!("会话 {} 本轮执行失败: {error}", handle.id);
                format!("本轮执行失败: {error}")
            }
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

    /// 投递并等待一轮；轮次超过 `progress_interval_secs` 未结束时向 `targets` 推送进度摘要。
    /// 同一会话同时只有一个汇报者（排队的后续投递不重复汇报）；汇报消息计入消息索引，可被 reply 直达该会话
    async fn deliver_reporting(
        &self,
        handle: &Arc<ManagedSession>,
        targets: &[String],
        prompt: &str,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        let interval = Duration::from_secs(self.config.progress_interval_secs);
        let reporter = !interval.is_zero()
            && self
                .reporting
                .lock()
                .expect("reporting")
                .insert(handle.id.clone());
        if !reporter {
            return handle.deliver_wait(prompt).await;
        }
        let result = progress::with_progress(
            handle.deliver_wait(prompt),
            interval,
            || handle.snapshot().session.clone(),
            |text| async move {
                for target in targets {
                    let ids = self.say(target, None, text.clone()).await;
                    let mut state = self.state.lock().await;
                    for id in ids {
                        if let Err(e) = state.index.record(&self.key(&id), &handle.id) {
                            tracing::warn!("记录进度消息索引失败: {e}");
                        }
                    }
                }
            },
        )
        .await;
        self.reporting.lock().expect("reporting").remove(&handle.id);
        result
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
                tracing::warn!("会话 {} rollover 失败: {error}", handle.id);
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
        tracing::info!(
            "推送 curator 报告: session={} status={:?}",
            report.session_id,
            report.result.status
        );
        if report.result.status == CuratorStatus::Updated {
            self.broadcast_memory_update(&report).await;
        }
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
            &super::curator::memory_dir(self.manager.workspace()),
            &report.result.summary,
        );
        let (mut total, mut failed) = (0, 0);
        for s in summaries.into_iter().filter(|s| {
            s.agent_managed()
                && s.workspace_root == self.manager.workspace()
                && s.kind != SessionKind::Curator
                && s.superseded_by.is_none()
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

    // ================== 定时任务 ==================

    /// 启动一次任务执行；同一任务仍在执行时跳过并返回 `false`
    pub async fn fire(self: &Arc<Self>, task: Task) -> bool {
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

    /// 任务回复的发送目标：session 来源（若有）+ bindings，去重保序
    async fn task_targets(&self, task: &Task) -> Vec<String> {
        let mut targets = Vec::new();
        if let Some(id) = &task.session
            && let Ok(handle) = self.manager.live_or_load(id).await
            && let Ok(binding) = Self::origin_binding(&handle, id)
        {
            targets.push(binding);
        }
        for binding in &task.bindings {
            if !targets.contains(binding) {
                targets.push(binding.clone());
            }
        }
        targets
    }

    async fn execute(self: &Arc<Self>, task: &Task) -> Result<(), Failure> {
        tracing::info!(
            "定时任务触发: id={} {} session={:?} bindings={:?}",
            task.id,
            task.trigger.describe(),
            task.session,
            task.bindings
        );
        let handle = match (&task.session, task.bindings.first()) {
            (Some(id), _) => self
                .manager
                .live_or_load(id)
                .await
                .map_err(|e| Failure::Permanent(format!("目标 session {id} 不可用: {e}")))?,
            (None, Some(binding)) => {
                self.origin(binding).map_err(Failure::Permanent)?;
                self.create(binding, SessionKind::Oneshot, "scheduled")
                    .await
                    .map_err(Failure::Transient)?
            }
            (None, None) => return Err(Failure::Permanent("任务没有执行目标".into())),
        };
        let targets = self.task_targets(task).await;
        let exec_binding = match (&task.session, targets.first()) {
            (Some(id), _) => Self::origin_binding(&handle, id).map_err(Failure::Permanent)?,
            (None, Some(binding)) => binding.clone(),
            (None, None) => return Err(Failure::Permanent("任务没有发送目标".into())),
        };
        let prompt = format_scheduled_reminder(&task.id, &task.trigger.describe(), &task.prompt);
        let limit = Duration::from_secs(self.config.task_timeout_secs);
        let report =
            match tokio::time::timeout(limit, self.deliver_reporting(&handle, &targets, &prompt))
                .await
            {
                Ok(result) => result.map_err(Failure::Transient)?,
                Err(_) => {
                    let _ = handle.abort_current().await;
                    return Err(Failure::Transient(format!(
                        "执行超过 {}s，已中止",
                        self.config.task_timeout_secs
                    )));
                }
            };
        let text = report.text.trim();
        if text == "[SILENT]" {
            tracing::info!("定时任务 {} 回复 [SILENT]，不通知", task.id);
        } else {
            let text = if text.is_empty() {
                "（本轮没有文本输出）"
            } else {
                text
            };
            for target in &targets {
                let ids = self.say(target, None, text).await;
                let mut state = self.state.lock().await;
                for id in ids {
                    if let Err(e) = state.index.record(&self.key(&id), &handle.id) {
                        tracing::warn!("记录出站消息索引失败: {e}");
                    }
                }
            }
        }
        self.after_turn(&handle, &exec_binding).await;
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

    fn origin_binding(handle: &Arc<ManagedSession>, id: &str) -> Result<String, String> {
        let session = handle.snapshot().session.clone();
        let origin = session
            .header
            .origin
            .as_ref()
            .ok_or_else(|| format!("session {id} 无 origin，无法回复"))?;
        Ok(format!("{}:{}", origin.channel, origin.chat_id))
    }

    /// 停机期间错过的任务：按聊天汇总提示一次，不补跑
    async fn report_missed(&self, missed: Vec<Missed>) {
        let mut by_binding: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for m in missed {
            let line = match m.task.trigger {
                Trigger::Once(_) => format!(
                    "{}（应于 {}，已错过；可 /schedule run {} 补跑）",
                    m.task.id,
                    m.last.format("%Y-%m-%d %H:%M"),
                    m.task.id
                ),
                _ => format!(
                    "{}（最近应于 {}，未补跑）",
                    m.task.id,
                    m.last.format("%Y-%m-%d %H:%M")
                ),
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

/// 正文预览：最多 60 个字符，按字符边界截取并标注
/// 表格单元格中的技能描述：压平空白、转义 `|`，超过 200 个字符截断
fn skill_desc_cell(description: &str) -> String {
    const LIMIT: usize = 200;
    let flat = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let cell = match flat.char_indices().nth(LIMIT) {
        Some((end, _)) => format!("{}…", &flat[..end]),
        None => flat,
    };
    cell.replace('|', "\\|")
}

fn preview(text: &str) -> String {
    match text.char_indices().nth(60) {
        Some((end, _)) => format!("{}…（共 {} 字符）", &text[..end], text.chars().count()),
        None => text.to_string(),
    }
}

fn describe_route(route: &Route) -> String {
    match route {
        Route::Existing(id) => format!("action=existing session={id}"),
        Route::Guide(id) => format!("action=guide session={id}"),
        Route::New { .. } => "action=new".to_string(),
        Route::Oneshot => "action=oneshot".to_string(),
    }
}

/// 校验 agent 模式模型：可解析且声明 `limit.context`（rollover 需要）
fn check_mode_model(
    manager: &SessionManager,
    model: &str,
    variant: Option<String>,
) -> Result<ModeModel, String> {
    let config = manager.app_config();
    let resolved = config
        .resolve_model(model, variant.as_deref())
        .map_err(|e| e.to_string())?;
    if resolved.model_config.limit.is_none() {
        return Err(format!(
            "模型 {} 未声明 limit.context，rollover 无法判定",
            resolved.full_spec
        ));
    }
    Ok(ModeModel {
        model: resolved.full_spec,
        variant,
    })
}

/// 启动期：`model.json` 优先，否则取配置（`settings.agent.model` → `settings.default`）
fn load_mode_model(
    config: &AgentSpaceConfig,
    manager: &SessionManager,
) -> Result<ModeModel, String> {
    let path = manager.workspace().join(MODE_MODEL_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let saved: ModeModel = serde_json::from_str(&raw)
                .map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;
            check_mode_model(manager, &saved.model, saved.variant).map_err(|e| {
                format!(
                    "{}: {e}（删除该文件可恢复配置文件中的模型）",
                    path.display()
                )
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let model = config
                .model
                .clone()
                .unwrap_or_else(|| manager.app_config().settings.default.clone());
            Ok(ModeModel {
                model,
                variant: None,
            })
        }
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

fn spec_label(model: &str, variant: Option<&str>) -> String {
    variant.map_or(model.to_string(), |v| format!("{model} ({v})"))
}

fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::New(_) => "/new",
        Command::Switch(_) => "/switch",
        Command::Sessions => "/sessions",
        Command::Stop => "/stop",
        Command::Status => "/status",
        Command::Rename(_) => "/rename",
        Command::Model(_) => "/model",
        Command::Skills => "/skills",
        Command::Schedule(_) => "/schedule",
        Command::Delete(_) => "/delete",
        Command::Clear(_) => "/clear",
        Command::Help => "/help",
        Command::Unknown(_) => "/unknown",
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

/// IM 状态表：Markdown 双栏表格（项 / 值 / 项 / 值），按行优先成对排布
fn status_table(rows: &[(&str, String)]) -> String {
    let cell = |(k, v): &(&str, String)| format!("{k} | {}", v.replace('|', "\\|"));
    let mut out = String::from("| 项 | 值 | 项 | 值 |\n| --- | --- | --- | --- |");
    for pair in rows.chunks(2) {
        let right = pair.get(1).map_or(" | ".into(), cell);
        out.push_str(&format!("\n| {} | {right} |", cell(&pair[0])));
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
    fn skill_desc_cell_truncates_and_escapes() {
        assert_eq!(skill_desc_cell("短描述"), "短描述");
        assert_eq!(
            skill_desc_cell(&"字".repeat(250)),
            format!("{}…", "字".repeat(200))
        );
        assert_eq!(skill_desc_cell("a | b\nc"), "a \\| b c");
    }

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
        async fn decide(
            &self,
            _: &str,
            _: &str,
            _: &crate::agent_space::router::RouteInput,
        ) -> Result<ModelReply, String> {
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
        schedule: Arc<ScheduleStore>,
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
        let schedule = ScheduleStore::open(&dir).unwrap();
        let runtime = Runtime::new(
            manager.clone(),
            AgentSpaceConfig::default(),
            channel.clone(),
            router,
            schedule.clone(),
        )
        .unwrap();
        Fixture {
            runtime,
            schedule,
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
    async fn unknown_slash_command_is_rejected_not_sent_to_model() {
        let f = fixture().await;
        f.runtime.handle(inbound("m1", "/stat2")).await;
        let sent = f.channel.wait(1).await;
        assert!(sent[0].text.contains("未知命令 /stat2"), "{}", sent[0].text);
        assert!(f.session_ids().await.is_empty());
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn rename_clear_and_info_commands() {
        let f = fixture().await;
        f.runtime.handle(inbound("m1", "/new 第一个")).await;
        f.channel.wait(1).await;
        let id = f.focus().await.unwrap();

        f.runtime.handle(inbound("m2", "/rename 新标题")).await;
        let sent = f.channel.wait(2).await;
        assert!(sent[1].text.contains("新标题"), "{}", sent[1].text);
        let handle = f.manager.live_or_load(&id).await.unwrap();
        assert_eq!(
            handle.snapshot().session.header.title.as_deref(),
            Some("新标题")
        );

        f.runtime.handle(inbound("m3", "/help")).await;
        f.runtime.handle(inbound("m4", "/schedule list")).await;
        f.runtime.handle(inbound("m5", "/skills")).await;
        let sent = f.channel.wait(5).await;
        assert!(sent[2].text.contains("/clear"), "{}", sent[2].text);
        assert!(sent[3].text.contains("暂无定时任务"), "{}", sent[3].text);

        f.runtime.handle(inbound("m6", "/clear")).await;
        let sent = f.channel.wait(6).await;
        assert!(sent[5].text.contains("已删除"), "{}", sent[5].text);
        assert!(f.session_ids().await.is_empty());
        assert_eq!(f.focus().await, None);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn model_switch_targets_focus_session_or_mode() {
        let f = fixture().await;
        let file = f.dir.join(MODE_MODEL_FILE);
        // 无 focus：改 agent 模式并落盘
        f.runtime.handle(inbound("m1", "/model p/m high")).await;
        let sent = f.channel.wait(1).await;
        assert!(sent[0].text.contains("agent 模式"), "{}", sent[0].text);
        assert!(std::fs::read_to_string(&file).unwrap().contains("high"));
        // 未声明 limit.context / 不存在的模型显式拒绝
        f.runtime.handle(inbound("m2", "/model nope/x")).await;
        f.channel.wait(2).await;
        assert!(std::fs::read_to_string(&file).unwrap().contains("p/m"));

        // 有 focus：只改会话，不动 agent 模式
        f.runtime.handle(inbound("m3", "/new 你好")).await;
        f.channel.wait(3).await;
        std::fs::remove_file(&file).unwrap();
        f.runtime.handle(inbound("m3b", "/model list")).await;
        let sent = f.channel.wait(4).await;
        assert!(
            sent[3].text.contains("p/m [会话] [模式]"),
            "{}",
            sent[3].text
        );
        f.runtime.handle(inbound("m4", "/model p/m")).await;
        let sent = f.channel.wait(5).await;
        assert!(!sent[4].text.contains("agent 模式"), "{}", sent[4].text);
        assert!(!file.exists());
        // --global：两者都改
        f.runtime.handle(inbound("m5", "/model --global p/m")).await;
        let sent = f.channel.wait(6).await;
        assert!(sent[5].text.contains("agent 模式"), "{}", sent[5].text);
        assert!(file.exists());
        // 仅 --variant：沿用当前模型只改档位
        f.runtime
            .handle(inbound("m7", "/model --variant high"))
            .await;
        let sent = f.channel.wait(7).await;
        assert!(sent[6].text.contains("p/m (high)"), "{}", sent[6].text);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn scheduled_task_runs_in_oneshot_session_and_replies_to_binding() {
        let f = fixture().await;
        let task: Task = serde_json::from_str(
            r#"{"id":"water","cron":"* * * * *","prompt":"喝水","bindings":["fake:c2c:u1"]}"#,
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

    fn task(json: &str) -> Task {
        serde_json::from_str(json).unwrap()
    }

    #[tokio::test]
    async fn scheduled_reply_goes_to_every_binding() {
        let f = fixture().await;
        f.runtime
            .fire(task(
                r#"{"id":"multi","every":"1h","prompt":"汇报","bindings":["fake:c2c:u1","fake:group:g2"]}"#,
            ))
            .await;
        let sent = f.channel.wait(2).await;
        let mut bindings: Vec<_> = sent.iter().map(|m| m.binding.as_str()).collect();
        bindings.sort();
        assert_eq!(bindings, ["fake:c2c:u1", "fake:group:g2"]);
        assert!(sent.iter().all(|m| m.text == "pong"));
        // 只执行一次：只有一个 oneshot 会话，来源是第一个 binding
        let ids = f.session_ids().await;
        assert_eq!(ids.len(), 1);
        let origin = f
            .manager
            .live(&ids[0])
            .await
            .unwrap()
            .snapshot()
            .session
            .header
            .origin
            .clone();
        assert_eq!(origin.unwrap().chat_id, "c2c:u1");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn scheduled_failure_notifies_and_disables_on_permanent_error() {
        let f = fixture().await;
        let t = task(
            r#"{"id":"gone","every":"1h","prompt":"p","session":"sess_none0","bindings":["fake:c2c:u1"]}"#,
        );
        f.schedule.add(t.clone()).unwrap();
        f.runtime.fire(t).await;
        let sent = f.channel.wait(1).await;
        assert!(sent[0].text.contains("已停用"), "{}", sent[0].text);
        assert!(sent[0].text.contains("sess_none0"), "{}", sent[0].text);
        assert!(!f.schedule.list()[0].1.enabled);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn schedule_command_manages_tasks() {
        let f = fixture().await;
        f.runtime.handle(inbound("m1", "/schedule")).await;
        let sent = f.channel.wait(1).await;
        assert!(sent[0].text.contains("暂无"), "{}", sent[0].text);

        f.schedule
            .add(task(
                r#"{"id":"water1","every":"1h","prompt":"喝水","bindings":["fake:c2c:u1"]}"#,
            ))
            .unwrap();
        f.runtime.handle(inbound("m2", "/schedule list")).await;
        f.runtime.handle(inbound("m3", "/schedule pause wat")).await;
        f.runtime
            .handle(inbound("m4", "/schedule run water1"))
            .await;
        f.runtime
            .handle(inbound("m5", "/schedule resume water1"))
            .await;
        let sent = f.channel.wait(5).await;
        assert!(
            sent[1].text.contains("water1") && sent[1].text.contains("every 1h"),
            "{}",
            sent[1].text
        );
        assert!(sent[2].text.contains("已暂停"), "{}", sent[2].text);
        assert!(sent[3].text.contains("已停用"), "{}", sent[3].text);
        assert!(sent[4].text.contains("已恢复"), "{}", sent[4].text);

        f.runtime
            .handle(inbound("m6", "/schedule delete water1"))
            .await;
        let sent = f.channel.wait(6).await;
        assert!(sent[5].text.contains("已删除"), "{}", sent[5].text);
        assert!(f.schedule.list().is_empty());
        f.runtime.handle(inbound("m7", "/schedule nope")).await;
        let sent = f.channel.wait(7).await;
        assert!(sent[6].text.contains("用法"), "{}", sent[6].text);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn memory_update_broadcast_notes_eligible_sessions_without_a_turn() {
        use super::super::curator::CuratorResult;
        let f = fixture().await;
        let mut ids = BTreeMap::new();
        for (name, kind, superseded) in [
            ("topic", SessionKind::Topic, false),
            ("oneshot", SessionKind::Oneshot, false),
            ("curator", SessionKind::Curator, false),
            ("superseded", SessionKind::Topic, true),
            ("source", SessionKind::Topic, false),
        ] {
            let mut session = Session::new(f.dir.clone(), None);
            session.header.kind = kind;
            session.header.origin = Some(Origin {
                channel: "fake".into(),
                chat_id: "c2c:u1".into(),
            });
            if superseded {
                session.header.superseded_by = Some("sess_next".into());
            }
            f.manager.store().save_header(&session).await.unwrap();
            ids.insert(name, session.header.id.clone());
        }
        // 来源会话已加载（运行时句柄）与冷会话两条路径都要覆盖：预加载 topic
        f.manager.live_or_load(&ids["topic"]).await.unwrap();

        f.runtime
            .push_curator(CuratorReport {
                session_id: ids["source"].clone(),
                origin: None,
                result: CuratorResult {
                    status: CuratorStatus::Updated,
                    summary: "新增偏好主题".into(),
                },
            })
            .await;

        for (name, id) in &ids {
            let stored = f.manager.store().load_session(id).await.unwrap().unwrap();
            if matches!(*name, "topic" | "oneshot") {
                assert_eq!(stored.turns.len(), 1, "{name}");
                let text = stored.turns[0].text_content();
                assert!(text.starts_with("<system-reminder type=\"memory-updated\">"));
                assert!(text.contains("新增偏好主题"));
                assert!(text.contains("MEMORY.md"));
            } else {
                assert!(stored.turns.is_empty(), "{name}");
            }
        }
        // 广播不触发模型调用：已加载会话也没有助手轮次
        let live = f.manager.live(&ids["topic"]).await.unwrap().snapshot();
        assert!(
            live.session
                .turns
                .iter()
                .all(|t| t.role != crate::model::Role::Assistant)
        );
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}
