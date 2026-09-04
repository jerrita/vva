use async_trait::async_trait;
use chrono::Utc;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use super::{SessionStore, StoreError};
use crate::model::{JsonlRecord, Session, SessionHeader, SessionSummary, Turn};

pub struct JsonlSessionStore {
    base_dir: PathBuf,
}

impl JsonlSessionStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let base_dir = base_dir.into();
        fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.base_dir.join(format!("{}.jsonl", session_id))
    }
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn save_header(&self, session: &Session) -> Result<(), StoreError> {
        let path = self.session_path(&session.id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = File::create(&path)?;
        let header_record = JsonlRecord::Header(SessionHeader::from(session));
        let header_json = serde_json::to_string(&header_record)?;
        writeln!(file, "{}", header_json)?;

        for turn in &session.turns {
            let turn_record = JsonlRecord::Turn(turn.clone());
            let turn_json = serde_json::to_string(&turn_record)?;
            writeln!(file, "{}", turn_json)?;
        }

        file.flush()?;
        Ok(())
    }

    async fn append_turn(&self, session_id: &str, turn: &Turn) -> Result<(), StoreError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(StoreError::NotFound(session_id.to_string()));
        }

        let mut file = OpenOptions::new().create(false).append(true).open(&path)?;

        let turn_record = JsonlRecord::Turn(turn.clone());
        let turn_json = serde_json::to_string(&turn_record)?;
        writeln!(file, "{}", turn_json)?;
        file.flush()?;

        Ok(())
    }

    async fn load_session(&self, session_id: &str) -> Result<Option<Session>, StoreError> {
        let path = self.session_path(session_id);
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

        let session = Session {
            id: header.id,
            title: header.title,
            workspace_root: header.workspace_root,
            created_at: header.created_at,
            updated_at: Utc::now(),
            current_model: header.current_model,
            current_variant: header.current_variant,
            web_search: header.web_search,
            turns,
            meta: header.meta,
        };

        Ok(Some(session))
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let mut summaries = Vec::new();

        if !self.base_dir.exists() {
            return Ok(summaries);
        }

        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                && let Ok(file) = File::open(&path)
            {
                let reader = BufReader::new(file);
                let mut lines = reader.lines();
                if let Some(Ok(first_line)) = lines.next()
                    && let Ok(JsonlRecord::Header(header)) =
                        serde_json::from_str::<JsonlRecord>(&first_line)
                {
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
            .push(ContentPart::image_url("https://example.com/err.png", None));
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
