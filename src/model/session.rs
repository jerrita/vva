use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use super::message::Role;
use super::turn::Turn;

fn default_web_search() -> bool {
    true
}

fn default_team() -> bool {
    true
}

/// 会话功能开关：web 搜索与 team（subagent 派生 / 跨会话消息）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionToggles {
    pub web_search: bool,
    pub team: bool,
}

impl Default for SessionToggles {
    fn default() -> Self {
        Self {
            web_search: true,
            team: true,
        }
    }
}

/// 会话功能开关的局部覆写：`None` 保持现值
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToggleOverrides {
    pub web_search: Option<bool>,
    pub team: Option<bool>,
}

impl ToggleOverrides {
    pub fn is_empty(&self) -> bool {
        self.web_search.is_none() && self.team.is_none()
    }

    /// 在既有开关上叠加覆写
    pub fn apply(self, base: SessionToggles) -> SessionToggles {
        SessionToggles {
            web_search: self.web_search.unwrap_or(base.web_search),
            team: self.team.unwrap_or(base.team),
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
                team: toggles.team,
                editor: EditorKind::default(),
                parent: None,
                agent: None,
                depth: 0,
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
            team: self.header.team,
        }
    }

    pub fn set_toggles(&mut self, toggles: SessionToggles) {
        self.header.web_search = toggles.web_search;
        self.header.team = toggles.team;
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
    /// team 开关：关闭时不挂载 Agent / SendMessage / WaitSession 工具
    #[serde(default = "default_team")]
    pub team: bool,
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
}
impl SessionHeader {
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
    Header(SessionHeader),
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
}

impl SessionSummary {
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
        }
    }
}
