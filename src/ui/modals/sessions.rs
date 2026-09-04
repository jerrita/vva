use crate::model::SessionSummary;
use std::path::Path;

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
}

impl SessionsModalState {
    pub fn new(all_sessions: Vec<SessionSummary>, current_workspace: &Path) -> Self {
        let mut state = Self {
            all_sessions,
            filtered_sessions: Vec::new(),
            selected_index: 0,
            scope: ProjectScope::CurrentProject,
        };
        state.apply_filter(current_workspace);
        state
    }

    /// 切换作用域（当前项目 <-> 全量历史项目）
    pub fn toggle_scope(&mut self, current_workspace: &Path) {
        self.scope = match self.scope {
            ProjectScope::CurrentProject => ProjectScope::AllProjects,
            ProjectScope::AllProjects => ProjectScope::CurrentProject,
        };
        self.apply_filter(current_workspace);
    }

    /// 应用过滤规则并重新定位光标
    pub fn apply_filter(&mut self, current_workspace: &Path) {
        let canonical_cur = current_workspace.canonicalize().ok();

        self.filtered_sessions = match self.scope {
            ProjectScope::CurrentProject => self
                .all_sessions
                .iter()
                .filter(|s| {
                    if let (Some(c_cur), Ok(c_s)) =
                        (&canonical_cur, s.workspace_root.canonicalize())
                    {
                        &c_s == c_cur
                    } else {
                        s.workspace_root == current_workspace
                    }
                })
                .cloned()
                .collect(),
            ProjectScope::AllProjects => self.all_sessions.clone(),
        };

        if self.selected_index >= self.filtered_sessions.len() {
            self.selected_index = 0;
        }
    }

    pub fn next(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.filtered_sessions.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_sessions.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.filtered_sessions.get(self.selected_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    #[test]
    fn test_sessions_modal_filter_and_toggle() {
        let ws_a = PathBuf::from("/home/user/project_a");
        let ws_b = PathBuf::from("/home/user/project_b");

        let s1 = SessionSummary {
            id: "sess_1".into(),
            title: Some("Task A1".into()),
            workspace_root: ws_a.clone(),
            created_at: Utc::now(),
            turn_count: 2,
            current_model: "model_a".into(),
            current_variant: None,
        };

        let s2 = SessionSummary {
            id: "sess_2".into(),
            title: Some("Task A2".into()),
            workspace_root: ws_a.clone(),
            created_at: Utc::now(),
            turn_count: 5,
            current_model: "model_a".into(),
            current_variant: None,
        };

        let s3 = SessionSummary {
            id: "sess_3".into(),
            title: Some("Task B1".into()),
            workspace_root: ws_b.clone(),
            created_at: Utc::now(),
            turn_count: 1,
            current_model: "model_b".into(),
            current_variant: None,
        };

        let mut modal = SessionsModalState::new(vec![s1, s2, s3], &ws_a);

        // 默认按当前工程过滤
        assert_eq!(modal.scope, ProjectScope::CurrentProject);
        assert_eq!(modal.filtered_sessions.len(), 2);
        assert_eq!(modal.selected_session().unwrap().id, "sess_1");

        // 导航测试
        modal.next();
        assert_eq!(modal.selected_session().unwrap().id, "sess_2");
        modal.next();
        assert_eq!(modal.selected_session().unwrap().id, "sess_1"); // wrap around
        modal.previous();
        assert_eq!(modal.selected_session().unwrap().id, "sess_2");

        // 切换到 All Projects
        modal.toggle_scope(&ws_a);
        assert_eq!(modal.scope, ProjectScope::AllProjects);
        assert_eq!(modal.filtered_sessions.len(), 3);

        // 再次切换回 Current Project
        modal.toggle_scope(&ws_a);
        assert_eq!(modal.scope, ProjectScope::CurrentProject);
        assert_eq!(modal.filtered_sessions.len(), 2);
    }
}
