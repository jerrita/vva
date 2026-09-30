use super::fuzzy_matches;

/// /variant 模态弹窗展示的推理强度档位条目
#[derive(Debug, Clone)]
pub struct VariantItem {
    pub id: Option<String>,
    pub display_name: String,
}

/// 判断档位条目是否与当前激活档位匹配
fn is_active_variant(item: &VariantItem, active: Option<&str>) -> bool {
    match (&item.id, active) {
        (Some(id), Some(cur)) => id == cur,
        (None, None) => true,
        _ => false,
    }
}

/// 当前激活档位在选项列表中的下标（未命中返回 None）
pub fn active_variant_index(items: &[VariantItem], active: Option<&str>) -> Option<usize> {
    items
        .iter()
        .position(|item| is_active_variant(item, active))
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
        state.selected_index =
            active_variant_index(&state.filtered_variants, state.active_variant.as_deref())
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

impl_filterable_modal!(VariantsModalState, filtered_variants);
