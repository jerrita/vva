//! IM 斜杠命令执行：会话管理、模型切换、技能与定时任务查询、状态表。

use std::sync::Arc;

use super::Runtime;
use super::mode_model::check_mode_model;
use crate::agent_space::channel::Inbound;
use crate::agent_space::command::{self, Command};
use crate::agent_space::curator::{CuratorReport, CuratorStatus};
use crate::agent_space::rollover::{cache_hit_rate, context_tokens};
use crate::agent_space::router::resolve_prefix;
use crate::agent_space::scheduler::describe_task;
use crate::harness::ManagedSession;
use crate::model::ToggleOverrides;
use crate::ui::history::compute_stats;

const MODEL_USAGE: &str = "用法: /model [list|[--global] [<规格>] [--variant <档位>]]";
const SCHEDULE_USAGE: &str =
    "用法: /schedule [list] | pause|resume|delete|run <id>（创建请直接告诉 agent）";

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
        let reply = format!("正在整理记忆，完成后删除会话 {}", handle.id);
        let (this, binding) = (self.clone(), binding.to_string());
        tokio::spawn(async move {
            let id = &handle.id;
            let reply = match this.curate_then_remove(&handle).await {
                Ok(()) => format!("已整理并删除会话 {id}"),
                Err(error) => format!("删除失败，会话 {id} 已保留: {error}"),
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
        let mut lines = Vec::new();
        if let Some(handle) = focus {
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
        lines.join("\n")
    }

    /// 配置中全部模型规格，标注默认会话与 agent 模式当前所用
    fn model_list(&self, focus: Option<&ManagedSession>) -> String {
        let session = focus.map(|h| h.snapshot().session.header.current_model.clone());
        let mode = self.mode_model().model;
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
        lines
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
            .join("\n")
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
        let mut lines = Vec::new();
        if let Some(handle) = focus {
            handle
                .configure(Some(spec), Some(variant), ToggleOverrides::default())
                .await?;
            lines.push(format!(
                "默认会话 {} 已切换到 {label}（下一轮起生效，切换会使该会话缓存失效）",
                handle.id
            ));
        }
        if update_mode {
            self.save_mode_model(checked)?;
            lines.push(format!("agent 模式模型已设为 {label}（新会话使用）"));
        }
        Ok(lines.join("\n"))
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
                let (task, state) = self.schedule.get(id)?;
                if !state.enabled {
                    return Err(format!("任务 {} 已停用，请先 /schedule resume", task.id));
                }
                let id = task.id.clone();
                if self.fire(task).await {
                    Ok(format!("已触发 {id}"))
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
}

fn spec_label(model: &str, variant: Option<&str>) -> String {
    variant.map_or(model.to_string(), |v| format!("{model} ({v})"))
}

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

    #[test]
    fn skill_desc_cell_truncates_and_escapes() {
        assert_eq!(
            skill_desc_cell(&"字".repeat(250)),
            format!("{}…", "字".repeat(200))
        );
        assert_eq!(skill_desc_cell("a | b\nc"), "a \\| b c");
    }
}
