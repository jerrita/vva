use async_trait::async_trait;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::{SessionStore, StoreError};
use crate::model::{JsonlRecord, Session, SessionHeader, SessionSummary, Turn};

/// JSONL 追加写存储引擎。
///
/// 文件布局：首行 Header 记录 + 逐行 Turn 记录。Header 变更（标题生成、模型切换）
/// 时整体重写文件——JSONL 首行长度不定、无法原地替换，以低频写放大换取格式
/// 简单与崩溃一致性。磁盘 IO 均通过 `spawn_blocking` 执行，避免阻塞异步运行时。
pub struct JsonlSessionStore {
    base_dir: PathBuf,
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

fn session_path(base_dir: &Path, session_id: &str) -> PathBuf {
    base_dir.join(format!("{session_id}.jsonl"))
}

fn save_header_sync(base_dir: &Path, session: &Session) -> Result<(), StoreError> {
    let path = session_path(base_dir, &session.id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = File::create(&path)?;
    let header_record = JsonlRecord::Header(SessionHeader::from(session));
    let header_json = serde_json::to_string(&header_record)?;
    writeln!(file, "{header_json}")?;

    for turn in &session.turns {
        let turn_record = JsonlRecord::Turn(turn.clone());
        let turn_json = serde_json::to_string(&turn_record)?;
        writeln!(file, "{turn_json}")?;
    }

    file.flush()?;
    Ok(())
}

fn append_turn_sync(base_dir: &Path, session_id: &str, turn: &Turn) -> Result<(), StoreError> {
    let path = session_path(base_dir, session_id);
    if !path.exists() {
        return Err(StoreError::NotFound(session_id.to_string()));
    }

    let mut file = OpenOptions::new().create(false).append(true).open(&path)?;

    let turn_record = JsonlRecord::Turn(turn.clone());
    let turn_json = serde_json::to_string(&turn_record)?;
    writeln!(file, "{turn_json}")?;
    file.flush()?;

    Ok(())
}

fn load_session_sync(base_dir: &Path, session_id: &str) -> Result<Option<Session>, StoreError> {
    let path = session_path(base_dir, session_id);
    if !path.exists() {
        return Ok(None);
    }

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
            JsonlRecord::Turn(t) => turns.push(t),
            _ => return Err(StoreError::InvalidFormat("Expected Turn record".into())),
        }
    }

    Ok(Some(header.into_session(turns)))
}

fn delete_session_sync(base_dir: &Path, session_id: &str) -> Result<(), StoreError> {
    let path = session_path(base_dir, session_id);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn list_sessions_sync(base_dir: &Path) -> Result<Vec<SessionSummary>, StoreError> {
    let mut summaries = Vec::new();

    if !base_dir.exists() {
        return Ok(summaries);
    }

    for entry in fs::read_dir(base_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
            && let Ok(file) = File::open(&path)
        {
            let reader = BufReader::new(file);
            let mut lines = reader.lines();
            let first_line = lines.next();
            let parsed_header = first_line.and_then(|l| l.ok()).and_then(|line| {
                match serde_json::from_str::<JsonlRecord>(&line) {
                    Ok(JsonlRecord::Header(header)) => Some(header),
                    // 损坏的 Header 不再静默吞没：显式告警后跳过该文件
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
                // 统计后续行数（Turn 数量）
                let turn_count = lines
                    .map_while(Result::ok)
                    .filter(|l| !l.trim().is_empty())
                    .count();
                summaries.push(SessionSummary {
                    id: header.id,
                    title: header.title,
                    workspace_root: header.workspace_root,
                    created_at: header.created_at,
                    turn_count,
                    current_model: header.current_model,
                    current_variant: header.current_variant,
                    web_search: header.web_search,
                });
            }
        }
    }

    // 按创建时间倒序排列
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

        let mut session = Session::with_model(
            PathBuf::from("/tmp/repo"),
            Some("Bugfix task".into()),
            "a/gpt-5.6-terra",
            Some("high".into()),
            true,
        );
        session.meta.response = Some(ResponseSessionMeta {
            model: Some("gpt-5.6-terra".into()),
            instructions: Some("You are a helpful coding assistant".into()),
        });

        // 写入 Session 头部
        store.save_header(&session).await.expect("save header");

        // 追加 Turn 1 (User + Image)
        let mut user_msg = Message::user("Here is the error screenshot");
        user_msg
            .parts
            .push(ContentPart::image_url("https://example.com/err.png"));
        let user_turn = Turn::user_turn(user_msg, None);
        store
            .append_turn(&session.id, &user_turn)
            .await
            .expect("append user turn");

        // 追加 Turn 2 (Assistant with OpenAI response_id and WebSearchCall)
        let mut assistant_turn = Turn::assistant_turn(
            vec![Message::assistant(vec![
                ContentPart::WebSearchCall(WebSearchCall::completed(
                    "search_1",
                    "rust compiler issue",
                    vec![WebSearchSource::new("https://rust-lang.org", "Rust", None)],
                )),
                ContentPart::text("Found solution"),
            ])],
            Some(user_turn.id.clone()),
        );
        assistant_turn.meta.response = Some(ResponseTurnMeta::new("resp_001xyz"));
        store
            .append_turn(&session.id, &assistant_turn)
            .await
            .expect("append assistant turn");

        // 重新加载并校验
        let loaded = store
            .load_session(&session.id)
            .await
            .expect("load session")
            .expect("session found");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.title, Some("Bugfix task".into()));
        assert_eq!(loaded.current_model, "a/gpt-5.6-terra");
        assert_eq!(loaded.current_variant.as_deref(), Some("high"));
        assert!(loaded.web_search);
        assert_eq!(loaded.turns.len(), 2);
        assert_eq!(loaded.latest_openai_response_id(), Some("resp_001xyz"));

        // 检查摘要列表
        let summaries = store.list_sessions().await.expect("list sessions");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, session.id);
        assert_eq!(summaries[0].current_model, "a/gpt-5.6-terra");
        assert_eq!(summaries[0].current_variant.as_deref(), Some("high"));
        assert!(summaries[0].web_search);
        assert_eq!(summaries[0].turn_count, 2);

        // 清理临时目录
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
