//! 进度汇报：长轮次每隔固定时长向 IM 推送一段摘要。
//! 摘要由会话已完成的轮次确定性提炼（改动文件、命令意图、文字消息、读取/检索次数），不额外调用模型。

use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::markdown;
use crate::model::{ContentPart, Role, Session};
use crate::tools::names;

/// 每类条目在单条汇报中的展示上限，超出时单独列出剩余数量与总数
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
    /// Read / Glob / Grep（含 hashline 同类）/ web 检索等只读调用次数
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

/// V4A 补丁涉及的文件：(路径, 是否新建)；`*** Move to:` 目标计为修改
fn patch_files(input: &str) -> Vec<(String, bool)> {
    input
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            [
                ("*** Add File: ", true),
                ("*** Update File: ", false),
                ("*** Delete File: ", false),
                ("*** Move to: ", false),
            ]
            .into_iter()
            .find_map(|(prefix, created)| {
                line.strip_prefix(prefix)
                    .map(|path| (path.trim().to_string(), created))
            })
        })
        .collect()
}

/// hashline 补丁涉及的文件：(路径, 是否新建)；`[PATH#TAG]` 为修改，`[PATH]` 为新建，
/// `MV DEST` 的目标计为修改（裸文件名在源文件所在目录内重命名）
fn hashline_files(input: &str) -> Vec<(String, bool)> {
    let mut files = Vec::new();
    let mut current: Option<String> = None;
    for line in input.lines() {
        let line = line.trim();
        if let Some(body) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let (path, created) = match body.rsplit_once('#') {
                Some((path, _)) => (path, false),
                None => (body, true),
            };
            current = Some(path.to_string());
            files.push((path.to_string(), created));
        } else if let Some(dest) = line.strip_prefix("MV ") {
            let dest = dest.trim().trim_matches(['"', '\'']);
            let Some(source) = &current else { continue };
            let dest = if dest.contains('/') {
                dest.to_string()
            } else {
                Path::new(source).with_file_name(dest).display().to_string()
            };
            files.push((dest, false));
        }
    }
    files
}

impl Digest {
    fn call(&mut self, root: &Path, name: &str, arguments: &str) {
        // freeform 工具的参数是原文而非 JSON
        let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
        let str_arg = |key: &str| args.get(key).and_then(Value::as_str);
        match name {
            names::WRITE => {
                if let Some(path) = str_arg("path") {
                    self.created.insert(relative(root, path));
                }
            }
            names::EDIT => {
                if let Some(path) = str_arg("path") {
                    self.edited.insert(relative(root, path));
                }
            }
            names::APPLY_PATCH => self.record_files(root, patch_files(arguments)),
            names::HASHLINE_EDIT => self.record_files(root, hashline_files(arguments)),
            names::SHELL => {
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
            names::READ
            | names::HASHLINE_READ
            | names::GLOB
            | names::GREP
            | names::HASHLINE_GREP
            | "web_search"
            | "web_fetch" => {
                self.lookups += 1;
            }
            _ => self.others += 1,
        }
    }

    fn record_files(&mut self, root: &Path, files: Vec<(String, bool)>) {
        for (path, created) in files {
            let path = relative(root, &path);
            if created {
                self.created.insert(path);
            } else {
                self.edited.insert(path);
            }
        }
    }

    fn render(self) -> Option<String> {
        let mut rows = Vec::new();
        for (label, files) in [("新建文件", &self.created), ("修改文件", &self.edited)] {
            for file in files.iter().take(MAX_FILES) {
                rows.push([label.into(), file.clone()]);
            }
            if files.len() > MAX_FILES {
                rows.push([
                    label.into(),
                    format!("另 {} 个，共 {} 个", files.len() - MAX_FILES, files.len()),
                ]);
            }
        }
        for command in self.commands.iter().take(MAX_COMMANDS) {
            rows.push(["执行命令".into(), command.clone()]);
        }
        if self.commands.len() > MAX_COMMANDS {
            rows.push([
                "执行命令".into(),
                format!(
                    "另 {} 条，共 {} 条",
                    self.commands.len() - MAX_COMMANDS,
                    self.commands.len()
                ),
            ]);
        }
        if self.lookups > 0 {
            rows.push(["读取 / 检索".into(), format!("{} 次", self.lookups)]);
        }
        if self.others > 0 {
            rows.push(["其他工具".into(), format!("{} 次", self.others)]);
        }
        if self.errors > 0 {
            rows.push(["工具报错".into(), format!("{} 次", self.errors)]);
        }
        // 文字消息取最近几条
        let skip = self.texts.len().saturating_sub(MAX_TEXTS);
        for text in self.texts.into_iter().skip(skip) {
            rows.push(["最新消息".into(), text]);
        }
        (!rows.is_empty()).then(|| markdown::table("本段进展", ["项目", "内容"], rows))
    }
}

/// 汇报文本：本段有动作时附摘要，否则注明无新动作（让用户知道仍在运行）
fn report(session: &Session, from: usize, elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    let head = format!("### ⏳ 仍在处理（已 {minutes} 分钟）");
    match digest(session, from) {
        Some(body) => format!("{head}\n\n{body}"),
        None => format!("{head}\n\n本段暂无新的工具调用或文字输出。"),
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
            call(names::WRITE, r#"{"path":"/w/memory/a.md","content":"x"}"#),
            call(names::HASHLINE_EDIT, "[src/b.rs#AB12]\nPUT 1 x"),
            call(
                names::EDIT,
                r#"{"path":"/w/c.rs","old_string":"a","new_string":"b"}"#,
            ),
            call(
                names::SHELL,
                r#"{"intent":"运行测试","command":"cargo test"}"#,
            ),
            call(
                names::SHELL,
                r#"{"intent":"启动服务","command":"x","run_in_background":true}"#,
            ),
            call(names::READ, r#"{"path":"/w/z"}"#),
            call(names::GREP, r#"{"pattern":"p"}"#),
        ]));
        let out = digest(&s, 0).unwrap();
        assert!(out.contains("| 新建文件 | memory/a.md |"), "{out}");
        assert!(out.contains("| 修改文件 | c.rs |"), "{out}");
        assert!(out.contains("| 修改文件 | src/b.rs |"), "{out}");
        assert!(out.contains("| 执行命令 | 运行测试 |"), "{out}");
        assert!(out.contains("| 执行命令 | 启动服务（后台） |"), "{out}");
        assert!(out.contains("| 读取 / 检索 | 2 次 |"), "{out}");
        assert!(out.contains("| 最新消息 | 开始整理 |"), "{out}");
        assert!(!out.contains("cargo test"), "只汇报意图，不泄露命令原文");
    }

    #[test]
    fn digest_parses_multi_file_patches() {
        let mut s = session("/w");
        s.append_turn(assistant(vec![
            call(
                names::APPLY_PATCH,
                "*** Begin Patch\n*** Add File: /w/n.rs\n+x\n*** Update File: a.rs\n*** Move to: b.rs\n@@\n-1\n+2\n*** Delete File: c.rs\n*** End Patch",
            ),
            call(
                names::HASHLINE_EDIT,
                "[/w/new.rs]\n+hi\n[sub/o.rs#AB12]\nMV r.rs\n[x.rs#CD34]\nMV d/y.rs\n",
            ),
        ]));
        let out = digest(&s, 0).unwrap();
        for row in [
            "| 新建文件 | n.rs |",
            "| 新建文件 | new.rs |",
            "| 修改文件 | a.rs |",
            "| 修改文件 | b.rs |",
            "| 修改文件 | c.rs |",
            "| 修改文件 | sub/o.rs |",
            "| 修改文件 | sub/r.rs |",
            "| 修改文件 | d/y.rs |",
        ] {
            assert!(out.contains(row), "{row}\n{out}");
        }
    }

    #[test]
    fn digest_respects_cursor_and_limits() {
        let mut s = session("/w");
        s.append_turn(assistant(vec![text("旧消息")]));
        let from = s.turns.len();
        assert_eq!(digest(&s, from), None);
        let many: Vec<_> = (0..10)
            .map(|i| {
                call(
                    names::WRITE,
                    &format!(r#"{{"path":"f{i}.txt","content":""}}"#),
                )
            })
            .collect();
        s.append_turn(assistant(many));
        s.append_turn(assistant(vec![text(&"长".repeat(300))]));
        let out = digest(&s, from).unwrap();
        assert!(out.contains("另 2 个，共 10 个"), "{out}");
        assert!(!out.contains("旧消息"));
        assert!(out.contains('…'));
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
}
