use super::{FilterableModal, fuzzy_matches};

/// /variant 模态弹窗展示的推理强度档位条目
#[derive(Debug, Clone)]
pub struct VariantItem {
    pub id: Option<String>,
    pub display_name: String,
    pub is_default: bool,
}

/// 判断档位条目是否与当前激活档位匹配
fn is_active_variant(item: &VariantItem, active: Option<&str>) -> bool {
    match (&item.id, active) {
        (Some(id), Some(cur)) => id == cur,
        (None, None) => true,
        _ => false,
    }
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
        let mut state = Self {
            current_model,
            active_variant,
            variants,
            filtered_variants: Vec::new(),
            selected_index: 0,
            query: String::new(),
        };
        state.apply_filter();
        // 初始光标定位到当前激活档位
        state.selected_index = state
            .filtered_variants
            .iter()
            .position(|v| is_active_variant(v, state.active_variant.as_deref()))
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

    pub fn selected_variant(&self) -> Option<&VariantItem> {
        self.filtered_variants.get(self.selected_index)
    }
}

impl FilterableModal for VariantsModalState {
    fn filtered_len(&self) -> usize {
        self.filtered_variants.len()
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

        modal.previous();
        assert_eq!(modal.selected_index, 1); // low
        assert_eq!(modal.selected_variant().unwrap().id.as_deref(), Some("low"));
    }
}
