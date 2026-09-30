use super::fuzzy_matches;

/// /model 模态选择框展示的模型条目
#[derive(Debug, Clone)]
pub struct ModelItem {
    pub full_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub provider_type: String,
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

impl_filterable_modal!(ModelsModalState, filtered_models);
