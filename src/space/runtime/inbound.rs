//! 入站：附件落盘与缓冲、路由、会话创建，随后把轮次交给独立任务执行。

use std::sync::Arc;

use chrono::Utc;

use super::Runtime;
use crate::harness::ManagedSession;
use crate::model::{SessionKind, SessionSummary, SessionToggles};
use crate::space::binding::{AttachedFile, FileKind, in_chat_scope};
use crate::space::channel::Inbound;
use crate::space::channel::files::{sanitize_filename, unique_dest};
use crate::space::command;
use crate::space::markdown;
use crate::space::router::{Candidate, ReplyRef, Route, RouteInput, Routed, SessionTtl};

impl Runtime {
    /// 处理一条入站消息；失败原样回复用户
    pub(super) async fn handle(self: &Arc<Self>, msg: Inbound) {
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
            return self.command(msg, cmd).await.map_err(|error| {
                let detail = error
                    .lines()
                    .map(markdown::text)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                format!("⚠️ **命令未完成**\n\n{detail}")
            });
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

    /// 路由（`force_new` 跳过路由直接新建）并取得目标会话，更新 focus 与消息索引后后台执行轮次
    pub(super) async fn route_and_run(
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
                    focus: true,
                    ttl: SessionTtl::Normal,
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
        let handle = match &routed.route {
            Route::Existing(id) => self.manager.live_or_load(id).await?,
            Route::Guide(id) => {
                self.say(&msg.binding, Some(&msg.message_id), "已转达给运行中的会话")
                    .await;
                self.manager.live_or_load(id).await?
            }
            Route::New { ttl, .. } => {
                self.create(
                    &msg.binding,
                    SessionKind::Topic,
                    routed.routed_by,
                    Some(ttl.seconds()),
                )
                .await?
            }
        };
        let quoted = match &routed.route {
            Route::New { quoted, .. } => quoted.as_deref(),
            _ => None,
        };
        let intro = match &routed.route {
            Route::New { focus, .. } => Some(format!(
                "> decision: new{}, {}",
                if *focus { ", focus" } else { "" },
                handle.id
            )),
            _ => None,
        };
        let prompt = compose_prompt(&text, &files, quoted);
        {
            let mut state = self.state.lock().await;
            if !matches!(routed.route, Route::New { focus: false, .. }) {
                state
                    .bindings
                    .set_focus(&msg.binding, Some(handle.id.clone()))?;
            }
            state.index.record(&self.key(&msg.message_id), &handle.id)?;
        }
        tokio::spawn(self.clone().run_turn(
            handle,
            msg.binding.clone(),
            msg.message_id.clone(),
            prompt,
            intro,
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
        let referenced = {
            let state = self.state.lock().await;
            input
                .reply_to
                .as_ref()
                .and_then(|reply| state.index.get(&reply.message_id))
                .map(str::to_string)
        };
        if let Some(id) = referenced {
            let summaries = self
                .manager
                .store()
                .list_sessions()
                .await
                .map_err(|e| e.to_string())?;
            if summaries
                .iter()
                .any(|s| s.id == id && !in_chat_scope(&msg.binding, s.origin.as_ref()))
            {
                return Err("引用会话不属于当前聊天，请发送新消息或使用 /new <消息>".into());
            }
        }
        let state = self.state.lock().await;
        self.router
            .route(&input, &state.index)
            .await
            .map_err(|e| e.to_string())
    }

    /// 当前聊天范围内未被接替的 topic 会话。
    pub(super) async fn scoped_sessions(
        &self,
        binding: &str,
    ) -> Result<Vec<SessionSummary>, String> {
        Ok(self
            .manager
            .store()
            .list_sessions()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|s| {
                self.is_active_space_session(s)
                    && s.kind == SessionKind::Topic
                    && in_chat_scope(binding, s.origin.as_ref())
            })
            .collect())
    }

    /// 清理已不存在或范围外的 focus。
    pub(super) async fn scoped_focus(
        &self,
        binding: &str,
        summaries: &[SessionSummary],
    ) -> Result<Option<String>, String> {
        let mut state = self.state.lock().await;
        match state.bindings.focus(binding).map(str::to_string) {
            Some(f) if !summaries.iter().any(|s| s.id == f) => {
                state.bindings.set_focus(binding, None)?;
                Ok(None)
            }
            focus => Ok(focus),
        }
    }

    /// router 候选：按聊天范围过滤，并清理已不存在或范围外的 focus。
    pub(super) async fn candidates(&self, binding: &str) -> Result<Vec<Candidate>, String> {
        let summaries = self.scoped_sessions(binding).await?;
        let focus = self.scoped_focus(binding, &summaries).await?;
        let mut candidates = Vec::new();
        for s in summaries {
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

    /// 以 agent 模式模型新建会话，来源绑定到 `binding`
    pub(super) async fn create(
        &self,
        binding: &str,
        kind: SessionKind,
        routed_by: &str,
        ttl_secs: Option<u64>,
    ) -> Result<Arc<ManagedSession>, String> {
        let origin = self.origin(binding)?;
        let mode = self.mode_model();
        let created = self
            .manager
            .create_session_with(
                self.manager.workspace().to_path_buf(),
                mode.model.clone(),
                mode.variant,
                None,
                SessionToggles::default(),
                |header| {
                    header.kind = kind;
                    header.origin = Some(origin);
                    header.routed_by = Some(routed_by.to_string());
                    header.ttl_secs = ttl_secs;
                },
            )
            .await?;
        tracing::info!(
            "新建会话: id={} kind={kind:?} model={} binding={binding} routed_by={routed_by}",
            created.id,
            mode.model
        );
        Ok(created)
    }
}

/// 正文预览：最多 60 个字符，按字符边界截取并标注
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
