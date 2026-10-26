//! Curator：将对话增量沉淀为记忆与 skill。
//!
//! 由系统在清理前创建 `kind=curator` 的后台会话（非模型派生的 subagent），
//! 只处理游标之后的增量 turns；产出写入 `memory/`（`MEMORY.md` 领域索引 + 领域/主题文件，
//! 纳入 git，每次整理一个 commit），反复出现的工作流晋升为工作区 `.vva/skills` 下的 skill（仅 agent space 可见）。
//! 状态由 git 判定而非模型自述：工作区有变更即 `Updated`，否则 `Unchanged`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

use crate::agents::AgentDef;
use crate::harness::SessionManager;
use crate::model::{ContentPart, Origin, Role, SessionKind, SessionToggles, Turn};
use crate::storage::SessionStore;

/// 内置 curator agent 名
pub const CURATOR_AGENT: &str = "curator";

/// 记忆库目录名（位于 agent 工作区下）
const MEMORY_DIR: &str = "memory";
/// 游标文件与输入暂存目录（位于 agent 工作区，不进入记忆库 git）
const CURSORS_FILE: &str = "curator-cursors.json";
const INPUT_DIR: &str = "curator-input";
/// 增量记录中单段文本 / 工具参数 / 工具结果的字符上限
const TEXT_LIMIT: usize = 4000;
const TOOL_LIMIT: usize = 300;

const CURATOR_PROMPT: &str = "\
你是 vva 的记忆整理员（curator）。你不与用户对话，只负责把一段对话增量整理为长期记忆，\
并在合适时把反复出现的工作流晋升为 skill。

## 记忆库（按领域分三层）
- 位置由任务指定（绝对路径）。三层结构：
  - `MEMORY.md`（L1）：只列**领域**，每行一个：`- [领域名](<领域>/INDEX.md): 一句话范围说明`。它会被整份注入每个新会话，因此只列领域，不列具体条目。
  - `<领域>/INDEX.md`（L2）：列出该领域下的主题，每行一条：`- [主题](topic.md): 一句话说明`。
  - `<领域>/<topic>.md`（L3）：具体内容。
- 开始前先阅读 `MEMORY.md`（可能尚不存在），再检索相关领域与主题。
- 归类：优先更新已有主题；新主题优先归入已有领域；没有合适领域才新建，新建前必须先检索现有领域，不要建内容重叠的领域。
- 某个领域的 `INDEX.md` 变得冗长时，在其下再拆子领域，保持 `MEMORY.md` 精简。

## 记录什么
- 只记录有长期价值的信息：用户偏好与习惯、稳定事实（环境、路径、人物、别名）、\
已做的决策及理由、约定与约束、可复用的经验结论。
- 不记录：一次性任务过程、临时状态、工具输出原文、寒暄，以及密码、密钥、令牌等敏感凭据。
- 信息只出现一次、表述含糊或属于推测时，条目末尾标注“（未确认）”，不要当作事实写入。
- 新信息与已有记忆冲突时，保留两者并标注冲突与时间，不要静默覆盖。
- 每条记忆自包含；使用绝对日期，不写“昨天”“上周”。

## skill 晋升
对话或已有记忆表明某个工作流已重复出现至少两次且步骤稳定时，写成 skill：\
`<skills 目录>/<name>/SKILL.md`，格式为\n\
---\nname: <与目录同名，仅小写字母、数字、连字符>\ndescription: <一句话，说明何时使用>\n---\n\
正文写步骤与注意事项。同名 skill 已存在时做最小修改。\
晋升后在 `MEMORY.md` 追加一行“已晋升 skill: <name>”。

## 工作方式
- 只写记忆库目录与 skills 目录。改动已有文件前先阅读并做最小变更，新文件直接新建。
- 没有值得记录的内容时，不要修改任何文件。
- 最后用中文输出不超过 10 行的摘要：新增 / 更新了哪些主题与 skill（各一句话）；未修改则写“无需更新”。";

/// 内置 curator 定义：仅文件读写工具，无 Shell / 联网 / 子代理
pub fn curator_agent() -> AgentDef {
    AgentDef {
        name: CURATOR_AGENT.into(),
        description: "Distills conversation increments into long-term memory and skills.".into(),
        model: None,
        tools: crate::tools::names::FILE_TOOLS
            .iter()
            .map(|name| name.to_string())
            .collect(),
        system_prompt: Some(CURATOR_PROMPT.into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratorStatus {
    Updated,
    Unchanged,
    Failed,
}

/// 一次整理的结果：`Failed` 时 `summary` 为失败原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratorResult {
    pub status: CuratorStatus,
    pub summary: String,
}

impl CuratorResult {
    fn unchanged(summary: &str) -> Self {
        Self {
            status: CuratorStatus::Unchanged,
            summary: summary.into(),
        }
    }
}

/// 上层回调参数：整理结果及来源会话的 IM 位置（供推送摘要）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratorReport {
    pub session_id: String,
    pub origin: Option<Origin>,
    pub result: CuratorResult,
}

/// 执行 curator 任务的后端：生产为 `kind=curator` 会话，测试可替换
#[async_trait::async_trait]
pub(super) trait CuratorRunner: Send + Sync {
    /// 运行任务提示词，返回最终消息；`label` 用于会话标题
    async fn run(&self, prompt: &str, label: &str) -> Result<String, String>;

    /// 清理为 `label` 创建过的执行载体（curator 会话）；无载体的后端无需实现
    async fn cleanup(&self, _label: &str) -> Result<(), String> {
        Ok(())
    }
}

/// curator 会话标题：`cleanup` 据此找回某来源会话的全部整理会话
fn curator_title(label: &str) -> String {
    format!("[curator] {label}")
}

/// 每次创建 curator 会话时取模型：独立配置，或跟随可在运行期切换的 agent 模式模型
pub type ModelSource = Arc<dyn Fn() -> String + Send + Sync>;

/// 在 agent 工作区创建 curator 会话并等待其完成
struct SessionRunner {
    manager: Arc<SessionManager>,
    model: ModelSource,
}

#[async_trait::async_trait]
impl CuratorRunner for SessionRunner {
    async fn run(&self, prompt: &str, label: &str) -> Result<String, String> {
        let handle = self
            .manager
            .create_session_with(
                self.manager.workspace().to_path_buf(),
                (self.model)(),
                None,
                Some(curator_title(label)),
                SessionToggles::default(),
                |header| {
                    header.kind = SessionKind::Curator;
                    header.agent = Some(CURATOR_AGENT.into());
                },
            )
            .await?;
        handle.deliver_wait(prompt).await.map(|report| report.text)
    }

    async fn cleanup(&self, label: &str) -> Result<(), String> {
        let title = curator_title(label);
        let ids: Vec<String> = self
            .manager
            .store()
            .list_sessions()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|s| s.kind == SessionKind::Curator && s.title.as_deref() == Some(&title))
            .map(|s| s.id)
            .collect();
        for id in ids {
            let handle = self.manager.live_or_load(&id).await?;
            self.manager.delete_session(&handle).await?;
            tracing::debug!("已清理 curator 会话 {id}（来源 {label}）");
        }
        Ok(())
    }
}

/// agent 工作区下的记忆库目录
pub fn memory_dir(workspace: &Path) -> PathBuf {
    workspace.join(MEMORY_DIR)
}

/// 当前记忆索引 `MEMORY.md` 的快照 reminder 文本；索引不存在或全空白为 `None`，
/// 其余读取失败显式报错
pub fn memory_reminder(workspace: &Path) -> Result<Option<String>, String> {
    let dir = memory_dir(workspace);
    let path = dir.join("MEMORY.md");
    match std::fs::read_to_string(&path) {
        Ok(index) if index.trim().is_empty() => Ok(None),
        Ok(index) => Ok(Some(crate::system::format_memory_reminder(&dir, &index))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

pub struct Curator {
    runner: Arc<dyn CuratorRunner>,
    store: Arc<dyn SessionStore>,
    /// agent 工作区：`memory/`、游标与输入暂存均在其下
    root: PathBuf,
    skills_dir: PathBuf,
    /// 串行化整理：git 提交与游标文件不容并发
    lock: Mutex<()>,
}

impl Curator {
    pub(super) fn new(
        runner: Arc<dyn CuratorRunner>,
        store: Arc<dyn SessionStore>,
        root: PathBuf,
        skills_dir: PathBuf,
    ) -> Self {
        Self {
            runner,
            store,
            root,
            skills_dir,
            lock: Mutex::new(()),
        }
    }

    /// 生产构造：curator 会话由 `manager` 创建，skills 写入工作区 `.vva/skills`
    /// （项目级技能，仅 agent space 会话可发现，不污染全局 `~/.vva/skills`）
    pub fn for_manager(manager: &Arc<SessionManager>, model: ModelSource) -> Self {
        let root = manager.workspace().to_path_buf();
        let skills_dir = root.join(".vva").join("skills");
        Self::new(
            Arc::new(SessionRunner {
                manager: manager.clone(),
                model,
            }),
            manager.store().clone(),
            root,
            skills_dir,
        )
    }

    fn memory_dir(&self) -> PathBuf {
        memory_dir(&self.root)
    }

    fn cursors_path(&self) -> PathBuf {
        self.root.join(CURSORS_FILE)
    }

    /// 会话增量记录的暂存文件
    fn input_path(&self, session_id: &str) -> PathBuf {
        self.root.join(INPUT_DIR).join(format!("{session_id}.md"))
    }

    /// 整理会话尚未处理的增量；任何失败都以 `Failed` 返回，游标与记忆库保持不变
    pub async fn curate(&self, session_id: &str) -> CuratorResult {
        let result = match self.try_curate(session_id).await {
            Ok(result) => result,
            Err(summary) => CuratorResult {
                status: CuratorStatus::Failed,
                summary,
            },
        };
        tracing::info!(
            "curate 结束: session={session_id} 状态={:?} 摘要={}",
            result.status,
            result.summary.lines().next().unwrap_or_default()
        );
        result
    }

    async fn try_curate(&self, session_id: &str) -> Result<CuratorResult, String> {
        let _guard = self.lock.lock().await;
        let session = self
            .store
            .load_session(session_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("会话 {session_id} 不存在"))?;
        if !session.header.supports_curation() || !session.header.curation_requested {
            return Ok(CuratorResult::unchanged("会话未请求沉淀"));
        }
        let mut cursors = Cursors::load(self.cursors_path())?;
        let start = cursors.start(session_id, &session.turns);
        let end = session.turns.len();
        if start >= end {
            return Ok(CuratorResult::unchanged("无新增内容"));
        }
        let last = &session.turns[end - 1].id;
        let transcript = render_transcript(start, &session.turns[start..]);
        if transcript.is_empty() {
            cursors.advance(session_id, last)?;
            tracing::debug!("curator 游标推进(无可沉淀内容): session={session_id} -> {last}");
            return Ok(CuratorResult::unchanged("新增轮次不含可沉淀的对话内容"));
        }

        tracing::info!("curate 开始: session={session_id} turn 范围 [{start}, {end})");
        let memory = self.memory_dir();
        blocking({
            let memory = memory.clone();
            move || git::prepare(&memory)
        })
        .await?;
        let input = self.input_path(session_id);
        write_input(&input, &transcript)?;
        let prompt = format!(
            "整理会话 {session_id} 第 [{start}, {end}) 轮的增量对话。\n\
             - 增量对话记录（用文件读取工具阅读）：{}\n\
             - 记忆库目录：{}\n\
             - skills 目录：{}",
            input.display(),
            memory.display(),
            self.skills_dir.display()
        );
        let reply = self.runner.run(&prompt, session_id).await;
        let _ = std::fs::remove_file(&input);
        let summary = match reply {
            Ok(text) => text.trim().to_string(),
            Err(error) => {
                // 工作区在整理前是干净的，回滚只丢弃本次整理的半成品；
                // 回滚失败则半成品会在下次整理前被当作外部修改提交，须一并报告
                let error = format!("curator 执行失败: {error}");
                return Err(match blocking(move || git::rollback(&memory)).await {
                    Ok(()) => error,
                    Err(rollback) => format!("{error}；回滚记忆库失败: {rollback}"),
                });
            }
        };

        let message = format!("curator: {session_id} turns [{start}, {end})\n\n{summary}");
        let committed = blocking(move || git::commit_all(&memory, &message)).await?;
        tracing::info!("curator git commit: session={session_id} 产生提交={committed}");
        cursors.advance(session_id, last)?;
        tracing::debug!("curator 游标推进: session={session_id} -> {last}");
        let status = if committed {
            CuratorStatus::Updated
        } else {
            CuratorStatus::Unchanged
        };
        Ok(CuratorResult { status, summary })
    }

    /// 会话被删除后同步清理：游标、残留的整理输入与为它创建的 curator 会话。
    /// curator 会话清理失败只告警（交给 TTL 兜底），不影响来源会话的删除结果
    pub async fn forget(&self, session_id: &str) -> Result<(), String> {
        let _guard = self.lock.lock().await;
        Cursors::load(self.cursors_path())?.remove(session_id)?;
        let _ = std::fs::remove_file(self.input_path(session_id));
        if let Err(error) = self.runner.cleanup(session_id).await {
            tracing::warn!("清理 {session_id} 的 curator 会话失败: {error}");
        }
        Ok(())
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}

fn write_input(path: &Path, transcript: &str) -> Result<(), String> {
    let dir = path.parent().expect("input path has a parent");
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(path, transcript))
        .map_err(|e| format!("写入整理输入 {} 失败: {e}", path.display()))
}

/// 每个会话已整理到的最后一个 turn id；会话回退（Revert）后该 id 不存在，从头整理
struct Cursors {
    path: PathBuf,
    last: BTreeMap<String, String>,
}

impl Cursors {
    fn load(path: PathBuf) -> Result<Self, String> {
        let last = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("游标文件 {} 损坏: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("读取游标文件 {} 失败: {e}", path.display())),
        };
        Ok(Self { path, last })
    }

    fn start(&self, session_id: &str, turns: &[Turn]) -> usize {
        self.last
            .get(session_id)
            .and_then(|last| turns.iter().rposition(|turn| &turn.id == last))
            .map_or(0, |index| index + 1)
    }

    fn advance(&mut self, session_id: &str, last_turn: &str) -> Result<(), String> {
        self.last.insert(session_id.into(), last_turn.into());
        self.save()
    }

    fn remove(&mut self, session_id: &str) -> Result<(), String> {
        if self.last.remove(session_id).is_some() {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.last).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text)
            .map_err(|e| format!("写入游标文件 {} 失败: {e}", self.path.display()))
    }
}

fn clip(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => format!("{}…（已截断）", &text[..index]),
        None => text.to_string(),
    }
}

/// 把增量轮次渲染为可读记录：保留用户 / 助手文本，工具调用与结果仅留摘要；
/// system 与 reminder 注入轮次不属于对话内容。无可沉淀内容时返回空串。
fn render_transcript(first_index: usize, turns: &[Turn]) -> String {
    let mut out = String::new();
    for (offset, turn) in turns.iter().enumerate() {
        let speaker = match turn.role {
            Role::User if !crate::system::is_reminder_text(&turn.text_content()) => "用户",
            Role::Assistant => "助手",
            Role::Tool => "工具",
            _ => continue,
        };
        let mut lines = Vec::new();
        for part in turn.messages.iter().flat_map(|message| &message.parts) {
            match part {
                ContentPart::Text { text, .. } if !text.trim().is_empty() => {
                    lines.push(clip(text.trim(), TEXT_LIMIT));
                }
                ContentPart::ToolCall(call) => lines.push(format!(
                    "[调用 {}: {}]",
                    call.name,
                    clip(&call.arguments, TOOL_LIMIT)
                )),
                ContentPart::ToolResult(result) => lines.push(format!(
                    "[结果{}: {}]",
                    if result.is_error { "（出错）" } else { "" },
                    clip(&result.content, TOOL_LIMIT)
                )),
                _ => {}
            }
        }
        if !lines.is_empty() {
            out.push_str(&format!(
                "### 第 {} 轮 · {speaker}\n{}\n\n",
                first_index + offset,
                lines.join("\n")
            ));
        }
    }
    out
}

/// 记忆库 git 操作（std::process::Command，阻塞调用须在 spawn_blocking 中执行）
mod git {
    use super::*;

    pub(super) fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=vva-curator",
                "-c",
                "user.email=curator@vva.local",
            ])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .map_err(|e| format!("无法执行 git: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git {} 失败: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn is_dirty(dir: &Path) -> Result<bool, String> {
        run(dir, &["status", "--porcelain"]).map(|out| !out.trim().is_empty())
    }

    /// 首次使用时 `git init`；工作区有未提交修改（会话或人工直接写入）时先单独提交保存，
    /// 使后续回滚只丢弃本次整理的半成品；存在未解决的合并冲突时才拒绝整理
    pub fn prepare(dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("创建记忆库 {} 失败: {e}", dir.display()))?;
        if !dir.join(".git").exists() {
            run(dir, &["-c", "init.defaultBranch=main", "init"])?;
        }
        if !run(dir, &["ls-files", "--unmerged"])?.trim().is_empty() {
            return Err(format!(
                "记忆库 {} 存在未解决的合并冲突；请人工处理后重试",
                dir.display()
            ));
        }
        commit_all(dir, "curator: 保存整理前的外部修改").map(|_| ())
    }

    /// 提交全部变更；无变更返回 false
    pub fn commit_all(dir: &Path, message: &str) -> Result<bool, String> {
        if !is_dirty(dir)? {
            return Ok(false);
        }
        run(dir, &["add", "-A"])?;
        run(dir, &["commit", "-m", message])?;
        Ok(true)
    }

    /// 丢弃未提交的整理半成品（尚无首个提交时只清理未跟踪文件）
    pub fn rollback(dir: &Path) -> Result<(), String> {
        if run(dir, &["rev-parse", "--verify", "HEAD"]).is_ok() {
            run(dir, &["reset", "--hard"])?;
        }
        run(dir, &["clean", "-fd"]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, Session};
    use crate::storage::jsonl::JsonlSessionStore;

    type Script = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

    /// 脚本化后端：执行闭包，记录调用次数与被清理的 label
    struct Scripted {
        calls: std::sync::atomic::AtomicUsize,
        cleaned: std::sync::Mutex<Vec<String>>,
        script: Script,
    }

    #[async_trait::async_trait]
    impl CuratorRunner for Scripted {
        async fn run(&self, prompt: &str, _label: &str) -> Result<String, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (self.script)(prompt)
        }

        async fn cleanup(&self, label: &str) -> Result<(), String> {
            self.cleaned.lock().unwrap().push(label.into());
            Ok(())
        }
    }

    fn calls(runner: &Scripted) -> usize {
        runner.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("vva-curator-{}", uuid::Uuid::new_v4()))
    }

    async fn fixture(
        script: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
    ) -> (Curator, Arc<Scripted>, Arc<JsonlSessionStore>, PathBuf) {
        let root = tmp();
        let store = Arc::new(JsonlSessionStore::new(root.join("sessions")).unwrap());
        let runner = Arc::new(Scripted {
            calls: Default::default(),
            cleaned: Default::default(),
            script: Box::new(script),
        });
        let curator = Curator::new(
            runner.clone(),
            store.clone(),
            root.clone(),
            root.join("skills"),
        );
        (curator, runner, store, root)
    }

    async fn seed(store: &JsonlSessionStore, root: &Path, texts: &[(&str, &str)]) -> Session {
        let mut session = Session::new(root, None);
        session.header.origin = Some(crate::model::Origin {
            channel: "test".into(),
            chat_id: "chat".into(),
        });
        session.header.curation_requested = true;
        store.save_header(&session).await.unwrap();
        for (role, text) in texts {
            let turn = match *role {
                "user" => Turn::user_turn(Message::text(*text)),
                _ => Turn::assistant_turn(vec![Message::text(*text)]),
            };
            store.append_turn(&session.header.id, &turn).await.unwrap();
            session.append_turn(turn);
        }
        session
    }

    /// 脚本：向记忆库写入一个主题文件
    fn write_topic(prompt: &str) -> Result<String, String> {
        let memory = prompt
            .lines()
            .find_map(|line| line.strip_prefix("- 记忆库目录："))
            .unwrap();
        std::fs::write(Path::new(memory).join("MEMORY.md"), "- [偏好](pref.md)\n").unwrap();
        Ok("新增偏好主题".into())
    }

    #[tokio::test]
    async fn unrequested_and_ineligible_sessions_skip_curation_without_side_effects() {
        let (curator, runner, store, root) = fixture(write_topic).await;
        let mut session = seed(&store, &root, &[("user", "A reusable convention")]).await;
        for (requested, kind, parent, origin) in [
            (
                false,
                SessionKind::Topic,
                None,
                session.header.origin.clone(),
            ),
            (
                true,
                SessionKind::Oneshot,
                None,
                session.header.origin.clone(),
            ),
            (
                true,
                SessionKind::Curator,
                None,
                session.header.origin.clone(),
            ),
            (
                true,
                SessionKind::Topic,
                Some("sess_parent".into()),
                session.header.origin.clone(),
            ),
            (true, SessionKind::Topic, None, None),
        ] {
            session.header.curation_requested = requested;
            session.header.kind = kind;
            session.header.parent = parent;
            session.header.origin = origin;
            store.save_header(&session).await.unwrap();
            assert_eq!(
                curator.curate(&session.header.id).await.status,
                CuratorStatus::Unchanged
            );
            assert_eq!(calls(&runner), 0);
            assert!(!curator.memory_dir().exists());
            assert!(!curator.cursors_path().exists());
            assert!(!curator.input_path(&session.header.id).exists());
        }
        // 后续打标仍处理此前全部对话。
        session.header.origin = Some(crate::model::Origin {
            channel: "test".into(),
            chat_id: "chat".into(),
        });
        store.save_header(&session).await.unwrap();
        assert_eq!(
            curator.curate(&session.header.id).await.status,
            CuratorStatus::Updated
        );
        assert_eq!(calls(&runner), 1);
        assert!(
            store
                .load_session(&session.header.id)
                .await
                .unwrap()
                .unwrap()
                .header
                .curation_requested
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn curate_commits_once_and_cursor_makes_repeat_unchanged() {
        let (curator, runner, store, root) = fixture(write_topic).await;
        let session = seed(
            &store,
            &root,
            &[("user", "我喜欢深色主题"), ("assistant", "记住了")],
        )
        .await;

        let first = curator.curate(&session.header.id).await;
        assert_eq!(first.status, CuratorStatus::Updated, "{}", first.summary);
        assert_eq!(first.summary, "新增偏好主题");
        let log = git::run(&curator.memory_dir(), &["log", "--format=%s"]).unwrap();
        assert!(log.contains(&format!("curator: {} turns [0, 2)", session.header.id)));

        // 游标已推进：重复触发不再调用后端，也不产生新 commit
        let again = curator.curate(&session.header.id).await;
        assert_eq!(again.status, CuratorStatus::Unchanged);
        assert_eq!(calls(&runner), 1);
        assert_eq!(
            git::run(&curator.memory_dir(), &["rev-list", "--count", "HEAD"])
                .unwrap()
                .trim(),
            "1"
        );

        // 新增轮次只处理增量范围
        let turn = Turn::user_turn(Message::text("再补充一条"));
        store.append_turn(&session.header.id, &turn).await.unwrap();
        let third = curator.curate(&session.header.id).await;
        assert_eq!(
            third.status,
            CuratorStatus::Unchanged,
            "无文件变更为 unchanged"
        );
        assert_eq!(calls(&runner), 2);
        let cursors = Cursors::load(curator.cursors_path()).unwrap();
        assert_eq!(cursors.last[&session.header.id], turn.id);

        // 来源会话删除后同步清理游标、残留输入与 curator 会话
        std::fs::write(curator.input_path(&session.header.id), "left over").unwrap();
        curator.forget(&session.header.id).await.unwrap();
        assert!(
            Cursors::load(curator.cursors_path())
                .unwrap()
                .last
                .is_empty()
        );
        assert!(!curator.input_path(&session.header.id).exists());
        assert_eq!(*runner.cleaned.lock().unwrap(), [session.header.id]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn runner_failure_rolls_back_and_keeps_cursor() {
        let (curator, _, store, root) = fixture(|prompt| {
            write_topic(prompt)?;
            Err("model down".into())
        })
        .await;
        let session = seed(&store, &root, &[("user", "记一下"), ("assistant", "好")]).await;

        let result = curator.curate(&session.header.id).await;
        assert_eq!(result.status, CuratorStatus::Failed);
        assert!(result.summary.contains("model down"));
        // 半成品被丢弃，游标未推进
        assert!(!curator.memory_dir().join("MEMORY.md").exists());
        assert!(!curator.cursors_path().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn external_edits_are_committed_before_curation() {
        let (curator, runner, store, root) = fixture(write_topic).await;
        let session = seed(&store, &root, &[("user", "x"), ("assistant", "y")]).await;
        let memory = curator.memory_dir();
        std::fs::create_dir_all(&memory).unwrap();
        git::prepare(&memory).unwrap();
        std::fs::write(memory.join("manual.md"), "人工笔记").unwrap();

        let result = curator.curate(&session.header.id).await;
        assert_ne!(result.status, CuratorStatus::Failed, "{}", result.summary);
        assert_eq!(calls(&runner), 1);
        assert_eq!(
            std::fs::read_to_string(memory.join("manual.md")).unwrap(),
            "人工笔记"
        );
        let log = git::run(&memory, &["log", "--format=%s"]).unwrap();
        assert!(log.contains("保存整理前的外部修改"), "{log}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn reminder_only_increment_is_skipped_without_runner() {
        let (curator, runner, store, root) = fixture(write_topic).await;
        let session = seed(
            &store,
            &root,
            &[(
                "user",
                "<system-reminder type=\"skills\">\nx\n</system-reminder>",
            )],
        )
        .await;
        let result = curator.curate(&session.header.id).await;
        assert_eq!(result.status, CuratorStatus::Unchanged);
        assert_eq!(calls(&runner), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn transcript_numbers_turns_and_summarizes_tools() {
        let mut call = Turn::assistant_turn(vec![Message::text("查一下")]);
        call.messages[0]
            .parts
            .push(ContentPart::ToolCall(crate::model::ToolCall::new(
                "c1",
                crate::tools::names::READ,
                "{\"path\":\"a\"}",
            )));
        let result = Turn::new(
            Role::Tool,
            vec![Message::tool_result(crate::model::ToolResult::success(
                "c1",
                "x".repeat(500),
            ))],
        );
        let text = render_transcript(5, &[Turn::user_turn(Message::text("hi")), call, result]);
        assert!(text.contains("### 第 5 轮 · 用户\nhi"));
        assert!(text.contains("### 第 6 轮 · 助手\n查一下\n[调用 Read: {\"path\":\"a\"}]"));
        assert!(text.contains("已截断"));
        assert!(!text.contains(&"x".repeat(400)));
    }
}
