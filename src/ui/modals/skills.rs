use super::{FilterableModal, fuzzy_matches};
use crate::skills::Skill;

/// /skills 模态弹窗状态机：浏览已发现的 Agent Skills（只读，无确认动作）
#[derive(Debug, Clone)]
pub struct SkillsModalState {
    pub skills: Vec<Skill>,
    pub filtered_skills: Vec<Skill>,
    pub selected_index: usize,
    pub query: String,
}

impl SkillsModalState {
    pub fn new(skills: Vec<Skill>) -> Self {
        let mut state = Self {
            skills,
            filtered_skills: Vec::new(),
            selected_index: 0,
            query: String::new(),
        };
        state.apply_filter();
        state
    }

    pub fn apply_filter(&mut self) {
        self.filtered_skills = self
            .skills
            .iter()
            .filter(|skill| self.matches_query(skill))
            .cloned()
            .collect();
        if self.selected_index >= self.filtered_skills.len() {
            self.selected_index = 0;
        }
    }

    fn matches_query(&self, skill: &Skill) -> bool {
        self.query.is_empty()
            || fuzzy_matches(&self.query, &skill.name)
            || fuzzy_matches(&self.query, &skill.description)
            || fuzzy_matches(&self.query, &skill.path.to_string_lossy())
    }

    pub fn selected_skill(&self) -> Option<&Skill> {
        self.filtered_skills.get(self.selected_index)
    }
}

impl FilterableModal for SkillsModalState {
    fn filtered_len(&self) -> usize {
        self.filtered_skills.len()
    }

    fn selected_index_mut(&mut self) -> &mut usize {
        &mut self.selected_index
    }

    fn query_mut(&mut self) -> &mut String {
        &mut self.query
    }

    fn refilter(&mut self) {
        self.apply_filter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillSource;
    use std::path::PathBuf;

    fn skill(name: &str, description: &str, source: SkillSource) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            path: PathBuf::from(format!("/tmp/skills/{name}")),
            source,
            body: String::new(),
        }
    }

    #[test]
    fn empty_filter_resets_selection() {
        let mut modal = SkillsModalState::new(vec![skill("alpha", "A", SkillSource::Global)]);
        modal.next();
        modal.push_query('z');
        assert!(modal.filtered_skills.is_empty());
        assert_eq!(modal.selected_index, 0);
        assert!(modal.selected_skill().is_none());
    }
}
