//! IM 斜杠命令执行：会话管理、模型切换、技能与定时任务查询、状态表。

use std::sync::Arc;

use super::Runtime;
use super::mode_model::check_mode_model;
use crate::agent_space::channel::Inbound;
use crate::agent_space::command::{self, Command};
use crate::agent_space::curator::{CuratorReport, CuratorStatus};
use crate::agent_space::markdown;
use crate::agent_space::rollover::{cache_hit_rate, context_tokens};
use crate::agent_space::router::resolve_prefix;
use crate::agent_space::scheduler::{
    ScheduleStore, Task, TaskState, Trigger, format_time, now_minute,
};
use crate::harness::ManagedSession;
use crate::model::ToggleOverrides;
use crate::ui::history::compute_stats;

const MODEL_USAGE: &str =
    "用法：/model [<规格>] [--variant <档位>] [--global]；/model list 查看可用模型";
const SCHEDULE_USAGE: &str =
    "用法：/schedule [list] 或 /schedule <pause|resume|delete|run> <id>；创建请直接告诉 agent";

impl Runtime {
    pub(super) async fn command(
        self: &Arc<Self>,
        msg: &Inbound,
        cmd: Command,
    ) -> Result<(), String> {
        let binding = &msg.binding;
        tracing::info!("处理命令: {} binding={binding}", cmd.name());
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
                "⏹️ 已请求中止当前轮次。".to_string()
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
                format!("✅ 已删除会话 **{}**。", markdown::text(&handle.id))
            }
            Command::Help => command::help(),
            Command::Unknown(name) => {
                format!(
                    "⚠️ 未知命令 /{}\n\n{}",
                    markdown::text(&name),
                    command::help()
                )
            }
        };
        self.say(binding, Some(&msg.message_id), reply).await;
        Ok(())
    }

    async fn focus_id(&self, binding: &str) -> Option<String> {
        self.state
            .lock()
            .await
            .bindings
            .focus(binding)
            .map(str::to_string)
    }

    async fn focus_handle(&self, binding: &str) -> Result<Arc<ManagedSession>, String> {
        let id = self
            .focus_id(binding)
            .await
            .ok_or("当前没有默认会话，可直接发消息或 /new <消息> 新建")?;
        self.manager.live_or_load(&id).await
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
        let id = self.resolve(binding, prefix).await?;
        self.manager.live_or_load(&id).await
    }

    async fn resolve(&self, binding: &str, prefix: &str) -> Result<String, String> {
        let candidates = self.candidates(binding).await?;
        resolve_prefix(&candidates, prefix).map_err(|e| e.to_string())
    }

    async fn switch(&self, binding: &str, prefix: &str) -> Result<String, String> {
        let id = self.resolve(binding, prefix).await?;
        self.state
            .lock()
            .await
            .bindings
            .set_focus(binding, Some(id.clone()))?;
        Ok(format!("✅ 已切换到会话 **{}**。", markdown::text(&id)))
    }

    async fn list_sessions(&self, binding: &str) -> Result<String, String> {
        let mut candidates = self.candidates(binding).await?;
        if candidates.is_empty() {
            return Ok(
                "### 💬 会话列表\n\n暂无会话。直接发送消息，或使用 `/new <消息>` 新建。".into(),
            );
        }
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        let mut out = markdown::table(
            "💬 会话列表",
            ["会话 ID", "标题", "状态", "默认"],
            candidates.iter().map(|c| {
                [
                    c.id.clone(),
                    c.title.clone(),
                    if c.running { "运行中" } else { "空闲" }.into(),
                    if c.focus { "📌 当前" } else { "—" }.into(),
                ]
            }),
        );
        out.push_str("\n\n💡 使用 `/switch <前缀>` 切换默认会话。");
        Ok(out)
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
        let reply = format!(
            "⏳ 正在整理记忆，完成后删除会话 **{}**。",
            markdown::text(&handle.id)
        );
        let (this, binding) = (self.clone(), binding.to_string());
        tokio::spawn(async move {
            let id = &handle.id;
            let reply = match this.curate_then_remove(&handle).await {
                Ok(()) => format!("✅ 已整理并删除会话 **{}**。", markdown::text(id)),
                Err(error) => format!(
                    "⚠️ 删除失败，会话 **{}** 已保留。\n\n{}",
                    markdown::text(id),
                    markdown::text(&error)
                ),
            };
            this.say(&binding, None, reply).await;
        });
        Ok(reply)
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
        Ok(format!(
            "✅ 已重命名会话 **{}**。\n\n**新标题：** {}",
            markdown::text(&handle.id),
            markdown::text(title)
        ))
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
        let focus = match self.focus_id(binding).await {
            Some(id) => Some(self.manager.live_or_load(&id).await?),
            None => None,
        };
        match words[..] {
            [] if flag_variant.is_none() && global => {
                Err("用法: /model --global <规格> [档位]".into())
            }
            [] if flag_variant.is_none() => Ok(self.model_overview(focus.as_deref())),
            ["list"] => Ok(self.model_list(focus.as_deref())),
            _ if flag_variant.is_some() && words.len() <= 1 => {
                self.apply_model(focus, global, words.first().copied(), flag_variant)
                    .await
            }
            [spec] | [spec, _] if !spec.is_empty() => {
                self.apply_model(focus, global, Some(spec), words.get(1).copied())
                    .await
            }
            _ => Err(MODEL_USAGE.into()),
        }
    }

    /// 默认会话与 agent 模式各自的模型、档位与可用档位
    fn model_overview(&self, focus: Option<&ManagedSession>) -> String {
        let mode = self.mode_model();
        let mut rows = Vec::new();
        if let Some(handle) = focus {
            let header = handle.snapshot().session.header.clone();
            rows.push([
                format!("默认会话 {}", handle.id),
                header.current_model,
                header.current_variant.unwrap_or_else(|| "none".into()),
            ]);
        }
        rows.push([
            "Agent 模式（新会话）".into(),
            mode.model,
            mode.variant.unwrap_or_else(|| "none".into()),
        ]);
        let mut out = markdown::table(
            "🤖 当前模型",
            ["作用范围", "模型规格", "当前档位", "可用档位"],
            rows.into_iter().map(|[scope, model, variant]| {
                let variants = self.variants_label(&model);
                [scope, model, variant, variants]
            }),
        );
        out.push_str("\n\n💡 `/model list` 查看可用模型；`/model --variant <档位>` 切换档位，`none` 清除档位。");
        out
    }

    /// 配置中全部模型规格，标注默认会话与 agent 模式当前所用
    fn model_list(&self, focus: Option<&ManagedSession>) -> String {
        let session = focus.map(|h| h.snapshot().session.header.current_model.clone());
        let mode = self.mode_model().model;
        let session = session.as_deref();
        let mode = mode.as_str();
        let config = self.manager.app_config();
        let mut rows: Vec<[String; 5]> = config
            .providers
            .iter()
            .flat_map(|(provider, p)| {
                p.models.iter().map(move |(id, m)| {
                    let spec = format!("{provider}/{id}");
                    let context =
                        m.limit.as_ref().map_or("未声明（不可选用）".into(), |l| {
                            format!("{}k", l.context / 1000)
                        });
                    let variants = m
                        .variants
                        .as_ref()
                        .filter(|v| !v.is_empty())
                        .map_or("无".into(), |v| v.join(" / "));
                    let mut scopes = Vec::new();
                    if session == Some(spec.as_str()) {
                        scopes.push("默认会话");
                    }
                    if spec == mode {
                        scopes.push("Agent 模式");
                    }
                    let scope = if scopes.is_empty() {
                        "—".into()
                    } else {
                        scopes.join(" / ")
                    };
                    [spec, m.name.clone(), context, variants, scope]
                })
            })
            .collect();
        if rows.is_empty() {
            return "### 🤖 可用模型\n\n暂无已配置的模型。".into();
        }
        rows.sort();
        let mut out = markdown::table(
            "🤖 可用模型",
            ["模型规格", "名称", "上下文", "可用档位", "当前使用"],
            rows,
        );
        out.push_str("\n\n💡 `/model <规格> [档位]` 切换模型；加 `--global` 同时修改新会话模型。");
        out
    }

    /// 模型声明的可用档位（`/` 分隔），未声明时给出提示
    fn variants_label(&self, spec: &str) -> String {
        self.manager
            .app_config()
            .resolve_model(spec, None)
            .ok()
            .and_then(|m| m.model_config.variants.clone())
            .filter(|v| !v.is_empty())
            .map_or("无".into(), |v| v.join(" / "))
    }

    /// 切换模型与/或档位：`spec` 缺省时沿用当前模型；档位 `none` 清除
    async fn apply_model(
        &self,
        focus: Option<Arc<ManagedSession>>,
        global: bool,
        spec: Option<&str>,
        variant: Option<&str>,
    ) -> Result<String, String> {
        let spec = match (spec, &focus) {
            (Some(spec), _) => spec.to_string(),
            (None, Some(h)) if !global => h.snapshot().session.header.current_model.clone(),
            (None, _) => self.mode_model().model,
        };
        let variant = variant.filter(|v| *v != "none").map(str::to_string);
        let update_mode = global || focus.is_none();
        // 会话与模式都要求声明 limit.context，否则 rollover 无法判定
        let checked = check_mode_model(&self.manager, &spec, variant.clone())?;
        let label = spec_label(&spec, variant.as_deref());
        let mut rows = Vec::new();
        if let Some(handle) = focus {
            handle
                .configure(Some(spec), Some(variant), ToggleOverrides::default())
                .await?;
            rows.push([
                format!("默认会话 {}", handle.id),
                label.clone(),
                "下一轮起生效；会话缓存将失效".into(),
            ]);
        }
        if update_mode {
            self.save_mode_model(checked)?;
            rows.push(["Agent 模式".into(), label, "新会话使用".into()]);
        }
        Ok(markdown::table(
            "✅ 模型已更新",
            ["作用范围", "模型 / 档位", "生效说明"],
            rows,
        ))
    }

    fn skills(&self) -> String {
        let skills = crate::skills::discover_skills(self.manager.workspace());
        if skills.is_empty() {
            return "### 🧩 本地技能\n\n暂无技能。".into();
        }
        markdown::table(
            "🧩 本地技能",
            ["名称", "来源", "描述"],
            skills.iter().map(|s| {
                let source = match s.source {
                    crate::skills::SkillSource::Global => "全局",
                    crate::skills::SkillSource::Project => "项目",
                };
                [
                    s.name.clone(),
                    source.into(),
                    description_preview(&s.description),
                ]
            }),
        )
    }

    /// `/schedule [list]`、`pause|resume|delete|run <id 前缀>`；创建走 Schedule 工具（自然语言）
    async fn schedule(self: &Arc<Self>, arg: &str) -> Result<String, String> {
        let mut words = arg.split_whitespace();
        let action = words.next().unwrap_or("list");
        let id = words.next();
        if words.next().is_some() {
            return Err(SCHEDULE_USAGE.into());
        }
        match (action, id) {
            ("list", None) => {
                if let Some(Err(error)) = self.schedule.refresh() {
                    return Err(error);
                }
                let tasks = self.schedule.list();
                if tasks.is_empty() {
                    return Ok("### 🗓️ 定时任务\n\n暂无定时任务。直接告诉 agent 任务内容与执行时间即可创建。".into());
                }
                let details = tasks
                    .iter()
                    .map(|(t, s)| schedule_table(t, s))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                Ok(format!(
                    "### 🗓️ 定时任务\n\n共 **{}** 项 · 时间为本地时区\n\n{details}\n\n💡 `/schedule <pause|resume|delete|run> <id>` 管理任务；创建请直接告诉 agent。",
                    tasks.len()
                ))
            }
            ("pause", Some(id)) => Ok(format!(
                "⏸️ 已暂停任务 **{}**。",
                markdown::text(&self.schedule.set_enabled(id, false)?.id)
            )),
            ("resume", Some(id)) => Ok(format!(
                "▶️ 已恢复任务 **{}**。",
                markdown::text(&self.schedule.set_enabled(id, true)?.id)
            )),
            ("delete", Some(id)) => Ok(format!(
                "✅ 已删除任务 **{}**。",
                markdown::text(&self.schedule.remove(id)?.id)
            )),
            ("run", Some(id)) => {
                let (task, state) = self.schedule.get(id)?;
                if !state.enabled {
                    return Err(format!(
                        "任务 {} 已停用，请先 /schedule resume {}",
                        task.id, task.id
                    ));
                }
                let id = task.id.clone();
                if self.fire(task).await {
                    Ok(format!("▶️ 已触发任务 **{}**。", markdown::text(&id)))
                } else {
                    Err(format!("任务 {id} 正在执行"))
                }
            }
            _ => Err(SCHEDULE_USAGE.into()),
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
                "运行状态",
                if handle.is_running() {
                    "运行中"
                } else {
                    "空闲"
                }
                .into(),
            ),
            ("轮次", stats.total_turns.to_string()),
            ("模型", model.to_string()),
            ("供应商", provider.to_string()),
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
                "输出速度（token/s）",
                stats.tps.map_or("暂无数据".into(), |v| format!("{v:.1}")),
            ),
            ("缓存读取", stats.cache_read_tokens.to_string()),
            ("缓存写入", stats.cache_write_tokens.to_string()),
            (
                "缓存命中率",
                cache_hit_rate(&session)
                    .map_or("暂无数据".into(), |r| format!("{:.0}%", r * 100.0)),
            ),
            ("预估费用", format!("${:.4}", stats.estimated_cost_usd)),
            ("待附件", pending.to_string()),
        ];
        Ok(status_table(&rows))
    }
}

fn spec_label(model: &str, variant: Option<&str>) -> String {
    variant.map_or(model.to_string(), |v| format!("{model} ({v})"))
}

/// 描述预览：压平空白，超过 200 个字符时以省略号明确截断。
fn description_preview(description: &str) -> String {
    const LIMIT: usize = 200;
    let flat = description.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(LIMIT) {
        Some((end, _)) => format!("{}…", &flat[..end]),
        None => flat,
    }
}

/// IM 状态表使用两列，长路径与标题在窄屏上也便于阅读。
fn status_table(rows: &[(&str, String)]) -> String {
    markdown::table(
        "📊 会话状态",
        ["项目", "内容"],
        rows.iter()
            .map(|(key, value)| [key.to_string(), value.clone()]),
    )
}

/// 每项任务独立成表，避免目标、提示词和错误信息挤在同一行。
fn schedule_table(task: &Task, state: &TaskState) -> String {
    let status = match (state.enabled, &task.trigger) {
        (true, _) => "启用",
        (false, Trigger::Once(_)) if state.last_run.is_some() => "已完成",
        (false, _) => "停用",
    };
    let mut targets: Vec<String> = task.session.iter().map(|id| format!("会话 {id}")).collect();
    targets.extend(task.bindings.iter().cloned());
    let mut rows = vec![
        ["任务 ID".into(), task.id.clone()],
        ["状态".into(), status.into()],
        ["触发规则".into(), task.trigger.describe()],
        ["执行目标".into(), targets.join(" / ")],
        [
            "下次执行".into(),
            ScheduleStore::next_fire(task, state, now_minute()).map_or("—".into(), format_time),
        ],
        [
            "上次执行".into(),
            state.last_run.map_or("尚未执行".into(), format_time),
        ],
        ["任务内容".into(), description_preview(&task.prompt)],
    ];
    if let Some(error) = &state.last_error {
        rows.push(["连续失败".into(), format!("{} 次", state.failures)]);
        rows.push(["最近错误".into(), error.clone()]);
    }
    markdown::table("任务详情", ["项目", "内容"], rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_desc_cell_truncates_and_escapes() {
        assert_eq!(
            markdown::text(&description_preview(&"字".repeat(250))),
            format!("{}…", "字".repeat(200))
        );
        assert_eq!(
            markdown::text(&description_preview("a | b\nc")),
            "a \\| b c"
        );
    }
}
