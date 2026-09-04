use super::{FilterableModal, fuzzy_matches};

/// /model 模态选择框展示的模型条目
#[derive(Debug, Clone)]
pub struct ModelItem {
    pub full_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub provider_type: String,
    pub reasoning: bool,
    pub context_limit: Option<usize>,
    pub variants: Option<Vec<String>>,
}

/// /model 模态弹窗状态机
#[derive(Debug, Clone)]
pub struct ModelsModalState {
    pub models: Vec<ModelItem>,
    pub filtered_models: Vec<ModelItem>,
    pub selected_index: usize,
    pub query: String,
}

impl ModelsModalState {
    pub fn new(models: Vec<ModelItem>, current_model: &str) -> Self {
        let mut state = Self {
            models,
            filtered_models: Vec::new(),
            selected_index: 0,
            query: String::new(),
        };
        state.apply_filter();
        // 初始光标定位到当前活跃模型
        state.selected_index = state
            .filtered_models
            .iter()
            .position(|m| m.full_id == current_model || m.model_id == current_model)
            .unwrap_or(0);
        state
    }

    pub fn apply_filter(&mut self) {
        self.filtered_models = self
            .models
            .iter()
            .filter(|model| {
                self.query.is_empty()
                    || fuzzy_matches(&self.query, &model.full_id)
                    || fuzzy_matches(&self.query, &model.display_name)
                    || fuzzy_matches(&self.query, &model.provider_type)
            })
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    pub fn selected_model(&self) -> Option<&ModelItem> {
        self.filtered_models.get(self.selected_index)
    }
}

impl FilterableModal for ModelsModalState {
    fn filtered_len(&self) -> usize {
        self.filtered_models.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
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

    #[test]
    fn test_models_modal_navigation() {
        let m1 = ModelItem {
            full_id: "prov/model-1".into(),
            provider_id: "prov".into(),
            model_id: "model-1".into(),
            display_name: "Model One".into(),
            provider_type: "response".into(),
            reasoning: true,
            context_limit: Some(128000),
            variants: Some(vec!["low".into(), "high".into()]),
        };
        let m2 = ModelItem {
            full_id: "prov/model-2".into(),
            provider_id: "prov".into(),
            model_id: "model-2".into(),
            display_name: "Model Two".into(),
            provider_type: "anthropic".into(),
            reasoning: false,
            context_limit: Some(200000),
            variants: None,
        };

        let mut modal = ModelsModalState::new(vec![m1, m2], "prov/model-1");
        assert_eq!(modal.selected_index, 0);
        assert_eq!(modal.selected_model().unwrap().full_id, "prov/model-1");

        modal.next();
        assert_eq!(modal.selected_model().unwrap().full_id, "prov/model-2");

        modal.push_query('t');
        modal.push_query('w');
        assert_eq!(modal.filtered_models.len(), 1);
        assert_eq!(modal.selected_model().unwrap().full_id, "prov/model-2");
    }
}
