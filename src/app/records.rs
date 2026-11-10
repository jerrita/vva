use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vva_core::model::EditorKind;

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

/// agent space 会话类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// 普通主题会话（含所有非 agent space 会话）
    #[default]
    Topic,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingSessionConfig {
    pub editor: EditorKind,
    pub web_search: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceSessionRecord {
    #[serde(deserialize_with = "Option::deserialize")]
    pub origin: Option<Origin>,
    pub kind: SessionKind,
    #[serde(deserialize_with = "Option::deserialize")]
    pub routed_by: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub ttl_secs: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_activity: Option<DateTime<Utc>>,
    pub curation_requested: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "record",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Application {
    Coding,
    Space(SpaceSessionRecord),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePaths {
    pub scratch: PathBuf,
    pub tool_outputs: PathBuf,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecord {
    pub id: String,
    pub coding: CodingSessionConfig,
    pub application: Application,
    pub resources: ResourcePaths,
}
