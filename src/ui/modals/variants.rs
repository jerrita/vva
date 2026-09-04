use super::fuzzy_matches;

/// /variant 模态弹窗展示的推理强度档位条目
#[derive(Debug, Clone)]
pub struct VariantItem {
    pub id: Option<String>,
    pub display_name: String,
    pub is_default: bool,
}

/// /variant 模态弹窗状态机
#[derive(Debug, Clone)]
pub struct VariantsModalState {
    pub current_model: String,
    pub active_variant: Option<String>,
    pub variants: Vec<VariantItem>,
    pub filtered_variants: Vec<VariantItem>,
    pub selected_index: usize,
    pub query: String,
}

impl VariantsModalState {
    pub fn new(
        current_model: String,
        active_variant: Option<String>,
        variants: Vec<VariantItem>,
    ) -> Self {
        let selected_index = variants
            .iter()
            .position(|v| match (&v.id, &active_variant) {
                (Some(id), Some(cur)) => id == cur,
                (None, None) => true,
                _ => false,
            })
            .unwrap_or(0);

        let mut state = Self {
            current_model,
            active_variant,
            variants,
            filtered_variants: Vec::new(),
            selected_index,
            query: String::new(),
        };
        state.apply_filter();
        state.selected_index = state
            .filtered_variants
            .iter()
            .position(|v| match (&v.id, &state.active_variant) {
                (Some(id), Some(cur)) => id == cur,
                (None, None) => true,
                _ => false,
            })
            .unwrap_or(0);
        state
    }

    pub fn apply_filter(&mut self) {
        self.filtered_variants = self
            .variants
            .iter()
            .filter(|variant| {
                self.query.is_empty()
                    || fuzzy_matches(&self.query, &variant.display_name)
                    || variant
                        .id
                        .as_deref()
                        .is_some_and(|id| fuzzy_matches(&self.query, id))
            })
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    pub fn push_query(&mut self, c: char) {
        self.query.push(c);
        self.apply_filter();
    }

    pub fn pop_query(&mut self) {
        self.query.pop();
        self.apply_filter();
    }

    pub fn next(&mut self) {
        if !self.filtered_variants.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.filtered_variants.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.filtered_variants.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_variants.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    pub fn selected_variant(&self) -> Option<&VariantItem> {
        self.filtered_variants.get(self.selected_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_variants_modal_navigation() {
        let v1 = VariantItem {
            id: None,
            display_name: "none (default)".into(),
            is_default: true,
        };
        let v2 = VariantItem {
            id: Some("low".into()),
            display_name: "low".into(),
            is_default: false,
        };
        let v3 = VariantItem {
            id: Some("high".into()),
            display_name: "high".into(),
            is_default: false,
        };

        // 默认定位到 active_variant (high)
        let mut modal =
            VariantsModalState::new("prov/model-1".into(), Some("high".into()), vec![v1, v2, v3]);
        assert_eq!(modal.selected_index, 2);
        assert_eq!(
            modal.selected_variant().unwrap().id.as_deref(),
            Some("high")
        );

        modal.next();
        assert_eq!(modal.selected_index, 0); // wrap around to none
        assert_eq!(modal.selected_variant().unwrap().id, None);

        modal.previous();
        assert_eq!(modal.selected_index, 2); // wrap around back to high

        modal.previous();
        assert_eq!(modal.selected_index, 1); // low
        assert_eq!(modal.selected_variant().unwrap().id.as_deref(), Some("low"));
    }
}
