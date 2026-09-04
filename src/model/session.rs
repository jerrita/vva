use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use super::message::Role;
use super::meta::SessionMeta;
use super::turn::Turn;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub workspace_root: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 当前选定的模型规格标识 (例如 "a/gpt-5.6-terra")
    #[serde(default)]
    pub current_model: String,
    /// 当前选定的推理强度档位 (例如 Some("high"))
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_variant: Option<String>,
    /// 是否开启原生网络搜索 / 实时 Grounding (保持工具挂载与 prompt cache 一致)
    #[serde(default)]
    pub web_search: bool,
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub meta: SessionMeta,
}

impl Session {
    pub fn new(workspace_root: impl Into<PathBuf>, title: Option<String>) -> Self {
        Self::with_model(workspace_root, title, String::new(), None, false)
    }

    pub fn with_model(
        workspace_root: impl Into<PathBuf>,
        title: Option<String>,
        current_model: impl Into<String>,
        current_variant: Option<String>,
        web_search: bool,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: format!("sess_{}", Uuid::new_v4().simple()),
            title,
            workspace_root: workspace_root.into(),
            created_at: now,
            updated_at: now,
            current_model: current_model.into(),
            current_variant,
            web_search,
            turns: Vec::new(),
            meta: SessionMeta::default(),
        }
    }

    pub fn last_turn(&self) -> Option<&Turn> {
        self.turns.last()
    }

    pub fn last_turn_id(&self) -> Option<String> {
        self.turns.last().map(|t| t.id.clone())
    }

    pub fn append_turn(&mut self, turn: Turn) {
        self.updated_at = Utc::now();
        self.turns.push(turn);
    }

    /// 更新会话当前绑定的模型与推理强度档位
    pub fn set_model(&mut self, model: impl Into<String>, variant: Option<String>) {
        self.current_model = model.into();
        self.current_variant = variant;
        self.updated_at = Utc::now();
    }

    /// 更新会话原生网络搜索开启状态
    pub fn set_web_search(&mut self, web_search: bool) {
        self.web_search = web_search;
        self.updated_at = Utc::now();
    }

    /// 提取最近一次成功且可链式调用的 OpenAI response_id
    pub fn latest_openai_response_id(&self) -> Option<&str> {
        for turn in self.turns.iter().rev() {
            if turn.role == Role::Assistant {
                return turn.openai_response_id();
            }
        }
        None
    }

    /// 在历史轮次中按 call_id 查找对应的 ToolCall
    pub fn find_tool_call(&self, call_id: &str) -> Option<&crate::model::ToolCall> {
        for turn in self.turns.iter().rev() {
            for msg in &turn.messages {
                for part in &msg.parts {
                    if let crate::model::ContentPart::ToolCall(call) = part
                        && call.id == call_id
                    {
                        return Some(call);
                    }
                }
            }
        }
        None
    }
}

/// JSONL 持久化格式：首行 Header
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionHeader {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub workspace_root: PathBuf,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub current_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_variant: Option<String>,
    #[serde(default)]
    pub web_search: bool,
    #[serde(default)]
    pub meta: SessionMeta,
}

impl From<&Session> for SessionHeader {
    fn from(s: &Session) -> Self {
        Self {
            id: s.id.clone(),
            title: s.title.clone(),
            workspace_root: s.workspace_root.clone(),
            created_at: s.created_at,
            current_model: s.current_model.clone(),
            current_variant: s.current_variant.clone(),
            web_search: s.web_search,
            meta: s.meta.clone(),
        }
    }
}

/// JSONL 记录条目（用于单行流式序列化）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonlRecord {
    Header(SessionHeader),
    Turn(Turn),
}

/// 用于列表展示的轻量会话摘要
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub workspace_root: PathBuf,
    pub created_at: DateTime<Utc>,
    pub turn_count: usize,
    pub current_model: String,
    pub current_variant: Option<String>,
    pub web_search: bool,
}
