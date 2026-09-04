pub mod jsonl;

use async_trait::async_trait;
use thiserror::Error;

use crate::model::{Session, SessionSummary, Turn};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Session not found: {0}")]
    NotFound(String),
    #[error("Invalid session format: {0}")]
    InvalidFormat(String),
    #[error("Blocking storage task failed: {0}")]
    TaskJoin(String),
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 保存或更新会话头部并初始化文件
    async fn save_header(&self, session: &Session) -> Result<(), StoreError>;

    /// 追加单个轮次到会话尾部 (Append-Only)
    async fn append_turn(&self, session_id: &str, turn: &Turn) -> Result<(), StoreError>;

    /// 加载完整会话数据
    async fn load_session(&self, session_id: &str) -> Result<Option<Session>, StoreError>;

    /// 删除会话及其持久化数据
    async fn delete_session(&self, session_id: &str) -> Result<(), StoreError>;

    /// 列出所有存储的会话概要
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError>;
}
