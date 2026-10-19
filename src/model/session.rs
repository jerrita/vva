use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use super::message::Role;
use super::turn::Turn;

fn default_web_search() -> bool {
    true
}

/// 会话功能开关：web 搜索
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionToggles {
    pub web_search: bool,
}

impl Default for SessionToggles {
    fn default() -> Self {
        Self { web_search: true }
    }
}

/// 会话功能开关的局部覆写：`None` 保持现值
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToggleOverrides {
    pub web_search: Option<bool>,
}

impl ToggleOverrides {
    pub fn is_empty(&self) -> bool {
        self.web_search.is_none()
    }

    /// 在既有开关上叠加覆写
    pub fn apply(self, base: SessionToggles) -> SessionToggles {
        SessionToggles {
            web_search: self.web_search.unwrap_or(base.web_search),
        }
    }
}

/// 会话绑定的文件编辑协议；创建时从模型 `annotation.editor` 写入，之后不随换模改变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorKind {
    Replace,
    Patch,
    #[default]
    Hashline,
}

fn missing_editor() -> EditorKind {
    // JSONL 缺省字段是早期 replace 会话；新会话默认 hashline，会显式写入。
    EditorKind::Replace
}

impl EditorKind {
    pub fn is_replace(&self) -> bool {
        matches!(self, Self::Replace)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Patch => "patch",
            Self::Hashline => "hashline",
        }
    }
}

/// 由用户首条 Prompt 派生的占位标题（正式标题由后台小模型异步生成后覆盖）
pub fn provisional_title(prompt: &str) -> String {
    prompt.chars().take(40).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// 持久化与投影的权威字段集：JSONL 首行、Web SessionInfo、SessionSummary 的唯一来源
    pub header: SessionHeader,
    pub updated_at: DateTime<Utc>,
    pub turns: Vec<Turn>,
}

fn is_zero_depth(depth: &u8) -> bool {
    *depth == 0
}

const SESSION_ID_SUFFIX_LEN: usize = 5;

/// `sess_` + 5 位小写十六进制
pub fn generate_session_id() -> String {
    let hex = Uuid::new_v4().simple().to_string();
    format!("sess_{}", &hex[..SESSION_ID_SUFFIX_LEN])
}

impl Session {
    pub fn new(workspace_root: impl Into<PathBuf>, title: Option<String>) -> Self {
        Self::with_model(
            workspace_root,
            title,
            String::new(),
            None,
            SessionToggles::default(),
        )
    }

    pub fn with_model(
        workspace_root: impl Into<PathBuf>,
        title: Option<String>,
        current_model: impl Into<String>,
        current_variant: Option<String>,
        toggles: SessionToggles,
    ) -> Self {
        let now = Utc::now();
        Self {
            header: SessionHeader {
                id: generate_session_id(),
                title,
                workspace_root: workspace_root.into(),
                created_at: now,
                current_model: current_model.into(),
                current_variant,
                web_search: toggles.web_search,
                editor: EditorKind::default(),
                parent: None,
                agent: None,
                depth: 0,
                kind: SessionKind::default(),
                origin: None,
                routed_by: None,
                continued_from: None,
                superseded_by: None,
                last_activity: None,
            },
            updated_at: now,
            turns: Vec::new(),
        }
    }

    pub fn append_turn(&mut self, turn: Turn) {
        self.updated_at = Utc::now();
        self.turns.push(turn);
    }

    /// 丢弃 `len` 之后的轮次（含），用于 Revert / Fork 截取前缀
    pub fn truncate_turns(&mut self, len: usize) {
        self.turns.truncate(len);
        self.updated_at = Utc::now();
    }

    /// 更新会话当前绑定的模型与推理强度档位
    pub fn set_model(&mut self, model: impl Into<String>, variant: Option<String>) {
        self.header.current_model = model.into();
        self.header.current_variant = variant;
        self.updated_at = Utc::now();
    }

    pub fn toggles(&self) -> SessionToggles {
        SessionToggles {
            web_search: self.header.web_search,
        }
    }

    pub fn set_toggles(&mut self, toggles: SessionToggles) {
        self.header.web_search = toggles.web_search;
        self.updated_at = Utc::now();
    }

    /// 提取最近一次成功且可链式调用的 OpenAI response_id
    /// 最近一个携带原生链锚点且亲和组匹配的 Assistant 轮次：
    /// 亲和组不一致（切换模型）时锚点不可复用，调用方回退全量历史重建
    pub fn latest_openai_response_id(&self, scope: Option<&str>) -> Option<&str> {
        self.turns.iter().rev().find_map(|turn| {
            if turn.role != Role::Assistant {
                return None;
            }
            let meta = turn.meta.response.as_ref()?;
            if meta.scope.as_deref() != scope {
                return None;
            }
            turn.openai_response_id()
        })
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
    #[serde(default = "default_web_search")]
    pub web_search: bool,
    #[serde(
        default = "missing_editor",
        skip_serializing_if = "EditorKind::is_replace"
    )]
    pub editor: EditorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_depth")]
    pub depth: u8,
    /// agent space 会话类别；缺省 Topic 不序列化
    #[serde(default, skip_serializing_if = "SessionKind::is_topic")]
    pub kind: SessionKind,
    /// 会话来源 IM 位置（agent space 创建的会话携带）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// 路由本会话的决策来源（如 `reply` / `switch` / `router`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_by: Option<String>,
    /// rollover 接替会话所承接的旧会话 id（与 subagent 的 `parent` 语义分离）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_from: Option<String>,
    /// rollover 后接替本会话的新会话 id；router 排除已被取代的会话
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// 最近一次用户活动时刻，TTL 清理依据（重启后据此恢复）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<DateTime<Utc>>,
}

/// agent space 会话类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// 普通主题会话（含所有非 agent space 会话）
    #[default]
    Topic,
    /// 一次性问答，不进入 router 候选，回复后清理
    Oneshot,
    /// 知识沉淀后台会话
    Curator,
}

impl SessionKind {
    pub fn is_topic(&self) -> bool {
        matches!(self, Self::Topic)
    }
}

/// IM 来源位置：channel 名与 chat id
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub channel: String,
    pub chat_id: String,
}

impl Origin {
    /// binding 键 `channel:chat_id`
    pub fn binding(&self) -> String {
        format!("{}:{}", self.channel, self.chat_id)
    }
}

/// agent space 会话携带 IM 来源或非 Topic 类别；TUI / Web 创建的普通会话两者皆无
fn is_agent_managed(kind: SessionKind, origin: Option<&Origin>) -> bool {
    origin.is_some() || !kind.is_topic()
}

impl SessionHeader {
    /// 是否为 agent space 管理的会话：仅此类会话维护 `last_activity`
    pub fn agent_managed(&self) -> bool {
        is_agent_managed(self.kind, self.origin.as_ref())
    }

    /// 由持久化 Header 与已加载的轮次重建运行时会话（updated_at 以加载时刻计）
    pub fn into_session(self, turns: Vec<Turn>) -> Session {
        Session {
            header: self,
            updated_at: Utc::now(),
            turns,
        }
    }
}

/// JSONL 记录条目（用于单行流式序列化）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonlRecord {
    Header(Box<SessionHeader>),
    Turn(Box<Turn>),
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
    /// 父会话 id（subagent 会话指向派生它的会话；顶层会话为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// 派生本会话的 agent 定义名（subagent 会话携带）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "SessionKind::is_topic")]
    pub kind: SessionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<DateTime<Utc>>,
}

impl SessionSummary {
    /// 是否为 agent space 管理的会话（TTL 清理的作用域）
    pub fn agent_managed(&self) -> bool {
        is_agent_managed(self.kind, self.origin.as_ref())
    }

    /// 由持久化 Header 与轮次条数构建列表摘要
    pub fn from_header(header: SessionHeader, turn_count: usize) -> Self {
        Self {
            id: header.id,
            title: header.title,
            workspace_root: header.workspace_root,
            created_at: header.created_at,
            turn_count,
            current_model: header.current_model,
            current_variant: header.current_variant,
            web_search: header.web_search,
            parent: header.parent,
            agent: header.agent,
            kind: header.kind,
            origin: header.origin,
            superseded_by: header.superseded_by,
            last_activity: header.last_activity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_header_loads_with_topic_defaults() {
        let line = r#"{"type":"header","id":"sess_a1b2c","workspace_root":"/w","created_at":"2026-01-01T00:00:00Z","current_model":"a/m"}"#;
        let JsonlRecord::Header(header) = serde_json::from_str(line).unwrap() else {
            panic!("expected header");
        };
        assert_eq!(header.kind, SessionKind::Topic);
        assert!(header.origin.is_none());
        assert!(header.routed_by.is_none() && header.superseded_by.is_none());
        assert!(header.last_activity.is_none());
        // 缺省字段不被序列化，旧格式保持稳定
        let out = serde_json::to_string(&JsonlRecord::Header(header)).unwrap();
        for key in [
            "kind",
            "origin",
            "routed_by",
            "superseded_by",
            "last_activity",
        ] {
            assert!(!out.contains(key), "{key} 不应被序列化");
        }
    }
}
