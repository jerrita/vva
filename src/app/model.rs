use super::records::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::Arc,
};
fn default_web_search() -> bool {
    true
}
fn is_zero_depth(depth: &u8) -> bool {
    *depth == 0
}
use vva_core::model::{self as core, EditorKind, Turn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionHeader {
    pub version: u32,
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
    /// 会话空闲 TTL（秒）；未指定时使用 agent space 配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    /// 当前会话请求在清理前沉淀；缺省不整理
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub curation_requested: bool,
}

impl SessionHeader {
    pub fn to_core(&self) -> core::SessionHeader {
        core::SessionHeader {
            version: self.version,
            id: self.id.clone(),
            title: self.title.clone(),
            workspace_root: self.workspace_root.clone(),
            created_at: self.created_at,
            current_model: self.current_model.clone(),
            current_variant: self.current_variant.clone(),
            parent: self.parent.clone(),
            agent: self.agent.clone(),
            depth: self.depth,
            continued_from: self.continued_from.clone(),
            superseded_by: self.superseded_by.clone(),
        }
    }
    pub fn agent_managed(&self) -> bool {
        self.origin.is_some() || self.kind == SessionKind::Curator
    }
    pub fn supports_curation(&self) -> bool {
        self.kind.is_topic() && self.agent_managed() && self.parent.is_none()
    }
    pub fn from_core(core: core::SessionHeader, record: &SessionRecord) -> Self {
        let (kind, origin, routed_by, last_activity, ttl_secs, curation_requested) =
            match &record.application {
                Application::Coding => (SessionKind::Topic, None, None, None, None, false),
                Application::Space(space) => (
                    space.kind,
                    space.origin.clone(),
                    space.routed_by.clone(),
                    space.last_activity,
                    space.ttl_secs,
                    space.curation_requested,
                ),
            };
        Self {
            version: core.version,
            id: core.id,
            title: core.title,
            workspace_root: core.workspace_root,
            created_at: core.created_at,
            current_model: core.current_model,
            current_variant: core.current_variant,
            parent: core.parent,
            agent: core.agent,
            depth: core.depth,
            continued_from: core.continued_from,
            superseded_by: core.superseded_by,
            web_search: record.coding.web_search,
            editor: record.coding.editor,
            kind,
            origin,
            routed_by,
            last_activity,
            ttl_secs,
            curation_requested,
        }
    }
    pub fn record(&self, resources: ResourcePaths) -> SessionRecord {
        let application = if self.agent_managed() {
            Application::Space(SpaceSessionRecord {
                origin: self.origin.clone(),
                kind: self.kind,
                routed_by: self.routed_by.clone(),
                last_activity: self.last_activity,
                ttl_secs: self.ttl_secs,
                curation_requested: self.curation_requested,
            })
        } else {
            Application::Coding
        };
        SessionRecord {
            id: self.id.clone(),
            coding: CodingSessionConfig {
                editor: self.editor,
                web_search: self.web_search,
            },
            application,
            resources,
        }
    }
    pub fn into_session(self, turns: Vec<Turn>) -> Session {
        let core = self.to_core().into_session(turns);
        Session {
            header: self,
            core: Arc::new(core),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub header: SessionHeader,
    pub(crate) core: Arc<core::Session>,
}
impl Deref for Session {
    type Target = core::Session;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
impl DerefMut for Session {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.core)
    }
}
impl Session {
    pub fn new(workspace: impl Into<PathBuf>, title: Option<String>) -> Self {
        Self::with_model(
            workspace,
            title,
            String::new(),
            None,
            SessionToggles::default(),
        )
    }
    pub fn with_model(
        workspace: impl Into<PathBuf>,
        title: Option<String>,
        model: impl Into<String>,
        variant: Option<String>,
        toggles: SessionToggles,
    ) -> Self {
        let core = core::Session::with_model(workspace, title, model, variant);
        let header = SessionHeader {
            version: core.header.version,
            id: core.header.id.clone(),
            title: core.header.title.clone(),
            workspace_root: core.header.workspace_root.clone(),
            created_at: core.header.created_at,
            current_model: core.header.current_model.clone(),
            current_variant: core.header.current_variant.clone(),
            parent: None,
            agent: None,
            depth: 0,
            continued_from: None,
            superseded_by: None,
            web_search: toggles.web_search,
            editor: EditorKind::default(),
            kind: SessionKind::Topic,
            origin: None,
            routed_by: None,
            last_activity: None,
            ttl_secs: None,
            curation_requested: false,
        };
        Self {
            core: Arc::new(core),
            header,
        }
    }
    pub fn from_core(core: Arc<core::Session>, record: &SessionRecord) -> Self {
        let header = SessionHeader::from_core(core.header.clone(), record);
        Self { core, header }
    }
    pub fn to_core(&self) -> core::Session {
        let mut session = (*self.core).clone();
        session.header = self.header.to_core();
        session
    }
    pub fn set_model(&mut self, model: impl Into<String>, variant: Option<String>) {
        Arc::make_mut(&mut self.core).set_model(model, variant);
        self.header.current_model = self.core.header.current_model.clone();
        self.header.current_variant = self.core.header.current_variant.clone();
    }
    pub fn toggles(&self) -> SessionToggles {
        SessionToggles {
            web_search: self.header.web_search,
        }
    }
    pub fn set_toggles(&mut self, toggles: SessionToggles) {
        self.header.web_search = toggles.web_search;
        Arc::make_mut(&mut self.core).updated_at = Utc::now();
    }
}
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub curation_requested: bool,
}

impl SessionSummary {
    pub fn agent_managed(&self) -> bool {
        self.origin.is_some() || self.kind == SessionKind::Curator
    }
    pub fn from_header(header: SessionHeader, count: usize) -> Self {
        Self {
            id: header.id,
            title: header.title,
            workspace_root: header.workspace_root,
            created_at: header.created_at,
            turn_count: count,
            current_model: header.current_model,
            current_variant: header.current_variant,
            web_search: header.web_search,
            parent: header.parent,
            agent: header.agent,
            kind: header.kind,
            origin: header.origin,
            routed_by: header.routed_by,
            superseded_by: header.superseded_by,
            last_activity: header.last_activity,
            ttl_secs: header.ttl_secs,
            curation_requested: header.curation_requested,
        }
    }
    pub fn from_core(core: core::SessionSummary, record: &SessionRecord) -> Self {
        let (kind, origin, routed_by, last_activity, ttl_secs, curation_requested) =
            match &record.application {
                Application::Coding => (SessionKind::Topic, None, None, None, None, false),
                Application::Space(space) => (
                    space.kind,
                    space.origin.clone(),
                    space.routed_by.clone(),
                    space.last_activity,
                    space.ttl_secs,
                    space.curation_requested,
                ),
            };
        Self {
            id: core.id,
            title: core.title,
            workspace_root: core.workspace_root,
            created_at: core.created_at,
            turn_count: core.turn_count,
            current_model: core.current_model,
            current_variant: core.current_variant,
            parent: core.parent,
            agent: core.agent,
            superseded_by: core.superseded_by,
            web_search: record.coding.web_search,
            kind,
            origin,
            routed_by,
            last_activity,
            ttl_secs,
            curation_requested,
        }
    }
}
