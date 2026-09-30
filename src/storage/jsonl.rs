use async_trait::async_trait;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::{SessionStore, StoreError};
use crate::model::{JsonlRecord, Session, SessionHeader, SessionSummary, Turn};

pub const SCRATCH_DIR: &str = "scratch";
pub const TOOL_OUTPUTS_DIR: &str = "tool_outputs";

/// JSONL 追加写存储引擎。
///
/// 文件布局：`{base}/{project_key}/{session_id}.jsonl`，同目录 `scratch/` 为项目便签、`tool_outputs/` 为大工具输出。
/// 首行 Header 记录 + 逐行 Turn 记录。Header 变更（标题生成、模型切换）
/// 时整体重写文件——JSONL 首行长度不定、无法原地替换，以低频写放大换取格式
/// 简单。重写经同目录临时文件落盘后 rename 替换，中途失败保留原文件。
/// 磁盘 IO 均通过 `spawn_blocking` 执行，避免阻塞异步运行时。
pub struct JsonlSessionStore {
    base_dir: PathBuf,
}

/// 将工作空间路径编码为文件系统安全的项目目录名
pub fn project_key(workspace: &Path) -> String {
    let resolved = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut key = String::new();
    for ch in resolved.to_string_lossy().chars() {
        match ch {
            c if c.is_control() || "/\\:<>\"|?*".contains(c) => {
                if !key.ends_with('-') {
                    key.push('-');
                }
            }
            c => key.push(c),
        }
    }
    let key = key.trim_matches('-').to_string();
    if key.is_empty() {
        "workspace".into()
    } else {
        key
    }
}

impl JsonlSessionStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let base_dir = base_dir.into();
        fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }
}

/// 在阻塞线程池上执行同步存储操作（JoinError 仅在内部 panic 时出现）
async fn run_blocking<T, F>(f: F) -> Result<T, StoreError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| StoreError::TaskJoin(e.to_string()))?
}

fn project_dir(base_dir: &Path, workspace: &Path) -> PathBuf {
    base_dir.join(project_key(workspace))
}

fn session_file_name(session_id: &str) -> String {
    format!("{session_id}.jsonl")
}

fn find_session_path(base_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let name = session_file_name(session_id);
    if let Ok(projects) = fs::read_dir(base_dir) {
        for project in projects.flatten() {
            let project_path = project.path();
            if !project_path.is_dir() {
                continue;
            }
            let candidate = project_path.join(&name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let legacy = base_dir.join(&name);
    legacy.is_file().then_some(legacy)
}

fn ensure_subdir_sync(project_dir: &Path, name: &str) -> Result<PathBuf, StoreError> {
    let path = project_dir.join(name);
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// 将 JSONL 记录序列化为单行写入
fn write_record(file: &mut File, record: &JsonlRecord) -> Result<(), StoreError> {
    writeln!(file, "{}", serde_json::to_string(record)?)?;
    Ok(())
}

fn save_header_sync(base_dir: &Path, session: &Session) -> Result<(), StoreError> {
    let dir = project_dir(base_dir, &session.header.workspace_root);
    let path = dir.join(session_file_name(&session.header.id));
    ensure_subdir_sync(&dir, SCRATCH_DIR)?;
    ensure_subdir_sync(&dir, TOOL_OUTPUTS_DIR)?;

    // 写入同目录临时文件后原子替换：重写中途失败时原文件与历史保持完整
    let temp_path = path.with_extension("jsonl.tmp");
    let mut file = File::create(&temp_path)?;
    write_record(&mut file, &JsonlRecord::Header(session.header.clone()))?;
    for turn in &session.turns {
        write_record(&mut file, &JsonlRecord::Turn(Box::new(turn.clone())))?;
    }

    file.flush()?;
    file.sync_all()?;
    drop(file);

    fs::rename(&temp_path, &path)?;

    let legacy = base_dir.join(session_file_name(&session.header.id));
    if legacy != path && legacy.is_file() {
        fs::remove_file(legacy)?;
    }
    Ok(())
}

fn append_turn_sync(base_dir: &Path, session_id: &str, turn: &Turn) -> Result<(), StoreError> {
    let path = find_session_path(base_dir, session_id)
        .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;

    let mut file = OpenOptions::new().create(false).append(true).open(&path)?;
    write_record(&mut file, &JsonlRecord::Turn(Box::new(turn.clone())))?;
    file.flush()?;

    Ok(())
}

fn load_session_sync(base_dir: &Path, session_id: &str) -> Result<Option<Session>, StoreError> {
    let Some(path) = find_session_path(base_dir, session_id) else {
        return Ok(None);
    };

    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let first_line = match lines.next() {
        Some(line) => line?,
        None => return Err(StoreError::InvalidFormat("Empty session file".into())),
    };

    let header: SessionHeader = match serde_json::from_str::<JsonlRecord>(&first_line)? {
        JsonlRecord::Header(h) => h,
        _ => {
            return Err(StoreError::InvalidFormat(
                "First line must be SessionHeader".into(),
            ));
        }
    };

    let mut turns = Vec::new();
    for line in lines {
        let line_str = line?;
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<JsonlRecord>(trimmed)? {
            JsonlRecord::Turn(t) => turns.push(*t),
            _ => return Err(StoreError::InvalidFormat("Expected Turn record".into())),
        }
    }

    Ok(Some(header.into_session(turns)))
}

/// 统计剩余字节流中的换行符数量（会话摘要只需轮次条数，无需解析内容）
fn count_lines(reader: &mut BufReader<File>) -> usize {
    let mut buffer = [0u8; 64 * 1024];
    let mut count = 0;
    while let Ok(read) = std::io::Read::read(reader, &mut buffer) {
        if read == 0 {
            break;
        }
        count += buffer[..read].iter().filter(|byte| **byte == b'\n').count();
    }
    count
}

fn delete_session_sync(base_dir: &Path, session_id: &str) -> Result<(), StoreError> {
    if let Some(path) = find_session_path(base_dir, session_id) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn collect_summaries(dir: &Path, summaries: &mut Vec<SessionSummary>) -> Result<(), StoreError> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        let parsed_header = reader.read_line(&mut first_line).ok().and_then(|_| {
            match serde_json::from_str::<JsonlRecord>(&first_line) {
                Ok(JsonlRecord::Header(header)) => Some(header),
                _ => {
                    eprintln!(
                        "warning: corrupt session header, skipped: {}",
                        path.display()
                    );
                    None
                }
            }
        });
        if let Some(header) = parsed_header {
            let turn_count = count_lines(&mut reader);
            summaries.push(SessionSummary::from_header(header, turn_count));
        }
    }
    Ok(())
}

fn list_sessions_sync(base_dir: &Path) -> Result<Vec<SessionSummary>, StoreError> {
    let mut summaries = Vec::new();

    if !base_dir.exists() {
        return Ok(summaries);
    }

    collect_summaries(base_dir, &mut summaries)?;
    for entry in fs::read_dir(base_dir)? {
        let project_path = entry?.path();
        if project_path.is_dir() {
            collect_summaries(&project_path, &mut summaries)?;
        }
    }

    summaries.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    Ok(summaries)
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn save_header(&self, session: &Session) -> Result<(), StoreError> {
        let base_dir = self.base_dir.clone();
        let session = session.clone();
        run_blocking(move || save_header_sync(&base_dir, &session)).await
    }

    async fn append_turn(&self, session_id: &str, turn: &Turn) -> Result<(), StoreError> {
        let base_dir = self.base_dir.clone();
        let session_id = session_id.to_string();
        let turn = turn.clone();
        run_blocking(move || append_turn_sync(&base_dir, &session_id, &turn)).await
    }

    async fn load_session(&self, session_id: &str) -> Result<Option<Session>, StoreError> {
        let base_dir = self.base_dir.clone();
        let session_id = session_id.to_string();
        run_blocking(move || load_session_sync(&base_dir, &session_id)).await
    }

    async fn delete_session(&self, session_id: &str) -> Result<(), StoreError> {
        let base_dir = self.base_dir.clone();
        let session_id = session_id.to_string();
        run_blocking(move || delete_session_sync(&base_dir, &session_id)).await
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let base_dir = self.base_dir.clone();
        run_blocking(move || list_sessions_sync(&base_dir)).await
    }

    async fn ensure_scratch(&self, workspace: &Path) -> Result<PathBuf, StoreError> {
        let dir = project_dir(&self.base_dir, workspace);
        run_blocking(move || ensure_subdir_sync(&dir, SCRATCH_DIR)).await
    }

    async fn ensure_tool_outputs(&self, workspace: &Path) -> Result<PathBuf, StoreError> {
        let dir = project_dir(&self.base_dir, workspace);
        run_blocking(move || ensure_subdir_sync(&dir, TOOL_OUTPUTS_DIR)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_jsonl_store_roundtrip_with_meta() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_test_{}", uuid::Uuid::new_v4().simple()));
        let store = JsonlSessionStore::new(&temp_dir).expect("init store");

        let session = Session::with_model(
            PathBuf::from("/tmp/repo"),
            Some("Bugfix task".into()),
            "a/gpt-5.6-terra",
            Some("high".into()),
            true,
        );

        // 写入 Session 头部
        store.save_header(&session).await.expect("save header");

        // 追加 Turn 1 (User + Image)
        let mut user_msg = Message::text("Here is the error screenshot");
        user_msg
            .parts
            .push(ContentPart::image_url("https://example.com/err.png"));
        let user_turn = Turn::user_turn(user_msg);
        store
            .append_turn(&session.header.id, &user_turn)
            .await
            .expect("append user turn");

        // 追加 Turn 2 (Assistant with OpenAI response_id and WebSearchCall)
        let mut assistant_turn = Turn::assistant_turn(vec![Message::new(vec![
            ContentPart::WebSearchCall(WebSearchCall::completed(
                "search_1",
                "rust compiler issue",
                vec![WebSearchSource::new("https://rust-lang.org", "Rust", None)],
            )),
            ContentPart::text("Found solution"),
        ])]);
        assistant_turn.meta.response = Some(ResponseTurnMeta::new("resp_001xyz"));
        store
            .append_turn(&session.header.id, &assistant_turn)
            .await
            .expect("append assistant turn");

        // 重新加载并校验
        let loaded = store
            .load_session(&session.header.id)
            .await
            .expect("load session")
            .expect("session found");
        assert_eq!(loaded.header.id, session.header.id);
        assert_eq!(loaded.header.title, Some("Bugfix task".into()));
        assert_eq!(loaded.header.current_model, "a/gpt-5.6-terra");
        assert_eq!(loaded.header.current_variant.as_deref(), Some("high"));
        assert_eq!(loaded.header.editor, crate::model::EditorKind::Hashline);
        assert!(loaded.header.web_search);
        assert_eq!(loaded.turns.len(), 2);
        assert_eq!(loaded.latest_openai_response_id(None), Some("resp_001xyz"));

        let project = temp_dir.join(project_key(Path::new("/tmp/repo")));
        assert!(
            project
                .join(format!("{}.jsonl", session.header.id))
                .is_file()
        );
        assert!(project.join(SCRATCH_DIR).is_dir());
        assert!(project.join(TOOL_OUTPUTS_DIR).is_dir());
        let scratch = store
            .ensure_scratch(Path::new("/tmp/repo"))
            .await
            .expect("scratch");
        assert_eq!(scratch, project.join(SCRATCH_DIR));
        let tool_outputs = store
            .ensure_tool_outputs(Path::new("/tmp/repo"))
            .await
            .expect("tool_outputs");
        assert_eq!(tool_outputs, project.join(TOOL_OUTPUTS_DIR));

        let summaries = store.list_sessions().await.expect("list sessions");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, session.header.id);
        assert_eq!(summaries[0].current_model, "a/gpt-5.6-terra");
        assert_eq!(summaries[0].current_variant.as_deref(), Some("high"));
        assert!(summaries[0].web_search);
        assert_eq!(summaries[0].turn_count, 2);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn sessions_from_different_projects_use_separate_dirs() {
        let temp_dir =
            std::env::temp_dir().join(format!("vva_test_{}", uuid::Uuid::new_v4().simple()));
        let store = JsonlSessionStore::new(&temp_dir).expect("init store");

        let a = Session::new(PathBuf::from("/tmp/project_a"), Some("A".into()));
        let b = Session::new(PathBuf::from("/tmp/project_b"), Some("B".into()));
        store.save_header(&a).await.expect("save a");
        store.save_header(&b).await.expect("save b");

        let dir_a = temp_dir.join(project_key(Path::new("/tmp/project_a")));
        let dir_b = temp_dir.join(project_key(Path::new("/tmp/project_b")));
        assert_ne!(dir_a, dir_b);
        assert!(dir_a.join(format!("{}.jsonl", a.header.id)).is_file());
        assert!(dir_b.join(format!("{}.jsonl", b.header.id)).is_file());
        assert!(dir_a.join(SCRATCH_DIR).is_dir());
        assert!(dir_b.join(SCRATCH_DIR).is_dir());

        let summaries = store.list_sessions().await.expect("list");
        assert_eq!(summaries.len(), 2);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn project_key_encodes_path_separators() {
        assert_eq!(
            project_key(Path::new("/no-such-vva-ws/user/proj")),
            "no-such-vva-ws-user-proj"
        );
        assert_eq!(project_key(Path::new("<>:\"|?*")), "workspace");
    }
}
