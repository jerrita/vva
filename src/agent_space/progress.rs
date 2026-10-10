//! 进度汇报：长轮次每隔固定时长向 IM 推送一段摘要。
//! 摘要由会话已完成的轮次确定性提炼（改动文件、命令意图、文字消息、读取/检索次数），不额外调用模型。

use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::model::{ContentPart, Role, Session};

/// 每类条目在单条汇报中的展示上限，超出以「等 N 项」收尾
const MAX_FILES: usize = 8;
const MAX_COMMANDS: usize = 6;
const MAX_TEXTS: usize = 3;
/// 单条文字消息的展示字符上限
const TEXT_LIMIT: usize = 120;

#[derive(Default)]
struct Digest {
    created: BTreeSet<String>,
    edited: BTreeSet<String>,
    commands: Vec<String>,
    texts: Vec<String>,
    /// Read / Glob / Grep / web 检索等只读调用次数
    lookups: usize,
    /// 其余工具（技能、定时任务、SendFile 等）的调用次数
    others: usize,
    errors: usize,
}

/// 汇总 `from` 之后的轮次；本段无任何动作时返回 `None`
fn digest(session: &Session, from: usize) -> Option<String> {
    let mut d = Digest::default();
    let root = Path::new(&session.header.workspace_root);
    for turn in session.turns.iter().skip(from) {
        for part in turn.messages.iter().flat_map(|m| &m.parts) {
            match part {
                ContentPart::Text { text, .. } if turn.role == Role::Assistant => {
                    let text = text.trim();
                    if !text.is_empty() {
                        d.texts.push(clip(text));
                    }
                }
                ContentPart::ToolCall(call) => d.call(root, &call.name, &call.arguments),
                ContentPart::ToolResult(result) if result.is_error => d.errors += 1,
                _ => {}
            }
        }
    }
    d.render()
}

fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TEXT_LIMIT {
        flat
    } else {
        format!("{}…", flat.chars().take(TEXT_LIMIT).collect::<String>())
    }
}

fn relative(root: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map_or_else(|_| path.to_string(), |p| p.display().to_string())
}

/// Hashline `[PATH#TAG]` 段头与 V4A `*** Update File:` 行中的路径
fn patch_paths(input: &str) -> Vec<String> {
    input
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            for prefix in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
                if let Some(path) = line.strip_prefix(prefix) {
                    return Some(path.trim().to_string());
                }
            }
            let body = line.strip_prefix('[')?.strip_suffix(']')?;
            let (path, tag) = body.rsplit_once('#')?;
            (!path.is_empty() && tag.len() == 4 && tag.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| path.to_string())
        })
        .collect()
}

impl Digest {
    fn call(&mut self, root: &Path, name: &str, arguments: &str) {
        let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
        let str_arg = |key: &str| args.get(key).and_then(Value::as_str);
        match name {
            "Write" => {
                if let Some(path) = str_arg("path") {
                    self.created.insert(relative(root, path));
                }
            }
            "Edit" => {
                let paths = match str_arg("input") {
                    Some(input) => patch_paths(input),
                    None => str_arg("path").map(str::to_string).into_iter().collect(),
                };
                self.edited
                    .extend(paths.iter().map(|path| relative(root, path)));
            }
            "Shell" => {
                let intent = str_arg("intent").unwrap_or("(未说明意图)");
                let background = args
                    .get("run_in_background")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.commands.push(if background {
                    format!("{intent}（后台）")
                } else {
                    intent.to_string()
                });
            }
            "Read" | "Glob" | "Grep" | "web_search" | "web_fetch" | "WebSearch" | "WebFetch" => {
                self.lookups += 1;
            }
            _ => self.others += 1,
        }
    }

    fn render(self) -> Option<String> {
        let mut lines = Vec::new();
        let files = |set: &BTreeSet<String>| {
            let shown: Vec<_> = set.iter().take(MAX_FILES).cloned().collect();
            let more = set.len().saturating_sub(MAX_FILES);
            if more > 0 {
                format!("{} 等 {} 个", shown.join("、"), set.len())
            } else {
                shown.join("、")
            }
        };
        if !self.created.is_empty() {
            lines.push(format!("新建文件: {}", files(&self.created)));
        }
        if !self.edited.is_empty() {
            lines.push(format!("修改文件: {}", files(&self.edited)));
        }
        if !self.commands.is_empty() {
            let more = self.commands.len().saturating_sub(MAX_COMMANDS);
            let mut text = format!(
                "执行命令: {}",
                self.commands
                    .iter()
                    .take(MAX_COMMANDS)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("；")
            );
            if more > 0 {
                text.push_str(&format!("（共 {} 条）", self.commands.len()));
            }
            lines.push(text);
        }
        if self.lookups + self.others > 0 {
            let mut parts = Vec::new();
            if self.lookups > 0 {
                parts.push(format!("读取/检索 {} 次", self.lookups));
            }
            if self.others > 0 {
                parts.push(format!("其他工具 {} 次", self.others));
            }
            lines.push(parts.join("，"));
        }
        if self.errors > 0 {
            lines.push(format!("工具报错 {} 次", self.errors));
        }
        // 文字消息取最近几条
        let skip = self.texts.len().saturating_sub(MAX_TEXTS);
        for text in self.texts.into_iter().skip(skip) {
            lines.push(format!("说: {text}"));
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

/// 汇报文本：本段有动作时附摘要，否则注明无新动作（让用户知道仍在运行）
fn report(session: &Session, from: usize, elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    let head = format!("⏳ 仍在处理（已 {minutes} 分钟）");
    match digest(session, from) {
        Some(body) => format!("{head}\n{body}"),
        None => format!("{head}\n本段暂无新的工具调用或文字输出"),
    }
}

/// 驱动 `work`，每隔 `interval` 对期间新增的轮次做摘要并交给 `send`；`work` 结束即返回其结果。
/// `session` 取会话当前快照；`interval` 为零表示不汇报。
pub async fn with_progress<T, Fut, S, F, SFut>(
    work: Fut,
    interval: Duration,
    session: S,
    mut send: F,
) -> T
where
    Fut: Future<Output = T>,
    S: Fn() -> Arc<Session>,
    F: FnMut(String) -> SFut,
    SFut: Future<Output = ()>,
{
    if interval.is_zero() {
        return work.await;
    }
    let started = tokio::time::Instant::now();
    let mut cursor = session().turns.len();
    let mut ticker = tokio::time::interval_at(started + interval, interval);
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return result,
            _ = ticker.tick() => {
                let snapshot = session();
                let text = report(&snapshot, cursor, started.elapsed());
                cursor = snapshot.turns.len();
                send(text).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolCall;
    use crate::model::{Message, Turn};
    use std::sync::Mutex;

    fn session(root: &str) -> Session {
        Session::new(root, None)
    }

    fn assistant(parts: Vec<ContentPart>) -> Turn {
        Turn::new(Role::Assistant, vec![Message::new(parts)])
    }

    fn call(name: &str, args: &str) -> ContentPart {
        ContentPart::ToolCall(ToolCall::new("c1", name, args))
    }

    fn text(t: &str) -> ContentPart {
        ContentPart::Text {
            text: t.into(),
            annotations: vec![],
        }
    }

    #[test]
    fn digest_lists_files_commands_and_texts() {
        let mut s = session("/w");
        s.append_turn(assistant(vec![
            text("开始整理"),
            call("Write", r#"{"path":"/w/memory/a.md","content":"x"}"#),
            call("Edit", r#"{"input":"[src/b.rs#AB12]\nPUT 1 x"}"#),
            call(
                "Edit",
                r#"{"path":"/w/c.rs","old_string":"a","new_string":"b"}"#,
            ),
            call("Shell", r#"{"intent":"运行测试","command":"cargo test"}"#),
            call(
                "Shell",
                r#"{"intent":"启动服务","command":"x","run_in_background":true}"#,
            ),
            call("Read", r#"{"path":"/w/z"}"#),
            call("Grep", r#"{"pattern":"p"}"#),
        ]));
        let out = digest(&s, 0).unwrap();
        assert!(out.contains("新建文件: memory/a.md"), "{out}");
        assert!(out.contains("修改文件: c.rs、src/b.rs"), "{out}");
        assert!(
            out.contains("执行命令: 运行测试；启动服务（后台）"),
            "{out}"
        );
        assert!(out.contains("读取/检索 2 次"), "{out}");
        assert!(out.contains("说: 开始整理"), "{out}");
        assert!(!out.contains("cargo test"), "只汇报意图，不泄露命令原文");
    }

    #[test]
    fn digest_respects_cursor_and_limits() {
        let mut s = session("/w");
        s.append_turn(assistant(vec![text("旧消息")]));
        let from = s.turns.len();
        assert_eq!(digest(&s, from), None);
        let many: Vec<_> = (0..10)
            .map(|i| call("Write", &format!(r#"{{"path":"f{i}.txt","content":""}}"#)))
            .collect();
        s.append_turn(assistant(many));
        s.append_turn(assistant(vec![text(&"长".repeat(300))]));
        let out = digest(&s, from).unwrap();
        assert!(out.contains("等 10 个"), "{out}");
        assert!(!out.contains("旧消息"));
        assert!(out.contains('…'));
    }

    #[test]
    fn report_notes_idle_segment() {
        let s = session("/w");
        let out = report(&s, 0, Duration::from_secs(185));
        assert!(
            out.contains("已 3 分钟") && out.contains("暂无新的"),
            "{out}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reports_each_interval_until_work_finishes() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sink = sent.clone();
        let snap = Arc::new(session("/w"));
        let out = with_progress(
            async {
                tokio::time::sleep(Duration::from_secs(400)).await;
                "done"
            },
            Duration::from_secs(180),
            move || snap.clone(),
            move |text| {
                let sink = sink.clone();
                async move { sink.lock().unwrap().push(text) }
            },
        )
        .await;
        assert_eq!(out, "done");
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].contains("已 3 分钟") && sent[1].contains("已 6 分钟"));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_interval_disables_reporting() {
        let snap = Arc::new(session("/w"));
        let out = with_progress(
            async {
                tokio::time::sleep(Duration::from_secs(1000)).await;
                1
            },
            Duration::ZERO,
            move || snap.clone(),
            |_| async { panic!("不应汇报") },
        )
        .await;
        assert_eq!(out, 1);
    }
}
