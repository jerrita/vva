use std::path::{Path, PathBuf};

use super::fuzzy_matches;
use crate::model::SessionSummary;

/// /sessions 会话列表筛选作用域
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectScope {
    CurrentProject,
    AllProjects,
}

/// /sessions 模态弹窗状态机
#[derive(Debug, Clone)]
pub struct SessionsModalState {
    pub all_sessions: Vec<SessionSummary>,
    pub filtered_sessions: Vec<SessionSummary>,
    pub selected_index: usize,
    pub scope: ProjectScope,
    pub query: String,
    /// 当前工程在全部会话中的数量（渲染 Tab 计数用，避免每帧重复统计）
    pub current_project_count: usize,
    /// 等待二次确认删除的会话 ID；确认期间锁定列表交互。
    pending_delete: Option<String>,
    current_workspace: PathBuf,
    /// 构造时缓存一次的规范化工作空间路径
    canonical_current: Option<PathBuf>,
}

impl SessionsModalState {
    pub fn new(all_sessions: Vec<SessionSummary>, current_workspace: &Path) -> Self {
        let mut state = Self {
            all_sessions,
            filtered_sessions: Vec::new(),
            selected_index: 0,
            scope: ProjectScope::CurrentProject,
            query: String::new(),
            current_project_count: 0,
            pending_delete: None,
            current_workspace: current_workspace.to_path_buf(),
            canonical_current: current_workspace.canonicalize().ok(),
        };
        state.apply_filter();
        state
    }

    /// 切换作用域（当前项目 <-> 全量历史项目）
    pub fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            ProjectScope::CurrentProject => ProjectScope::AllProjects,
            ProjectScope::AllProjects => ProjectScope::CurrentProject,
        };
        self.apply_filter();
    }

    /// 应用过滤规则并重新定位光标
    pub fn apply_filter(&mut self) {
        self.current_project_count = self
            .all_sessions
            .iter()
            .filter(|s| self.in_current_project(s))
            .count();

        self.filtered_sessions = match self.scope {
            ProjectScope::CurrentProject => self
                .all_sessions
                .iter()
                .filter(|s| self.in_current_project(s))
                .filter(|s| self.matches_query(s))
                .cloned()
                .collect(),
            ProjectScope::AllProjects => self
                .all_sessions
                .iter()
                .filter(|s| self.matches_query(s))
                .cloned()
                .collect(),
        };

        if self.selected_index >= self.filtered_sessions.len() {
            self.selected_index = 0;
        }
    }

    /// 会话是否属于当前工作空间（优先规范化路径比对，失败时退回字面比对）
    fn in_current_project(&self, session: &SessionSummary) -> bool {
        if let (Some(c_cur), Ok(c_s)) = (
            &self.canonical_current,
            session.workspace_root.canonicalize(),
        ) {
            &c_s == c_cur
        } else {
            session.workspace_root == self.current_workspace
        }
    }

    fn matches_query(&self, session: &SessionSummary) -> bool {
        self.query.is_empty()
            || fuzzy_matches(&self.query, &session.id)
            || session
                .title
                .as_deref()
                .is_some_and(|title| fuzzy_matches(&self.query, title))
            || fuzzy_matches(&self.query, &session.current_model)
            || fuzzy_matches(&self.query, &session.workspace_root.to_string_lossy())
    }

    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.filtered_sessions.get(self.selected_index)
    }

    pub fn request_delete(&mut self) {
        self.pending_delete = self.selected_session().map(|session| session.id.clone());
    }

    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    pub fn pending_delete(&self) -> Option<&str> {
        self.pending_delete.as_deref()
    }

    /// 从完整列表移除已删除的会话，并重新应用当前作用域与筛选条件。
    pub fn remove_session(&mut self, session_id: &str) {
        self.all_sessions.retain(|session| session.id != session_id);
        self.pending_delete = None;
        self.apply_filter();
    }
}

impl_filterable_modal!(SessionsModalState, filtered_sessions);
