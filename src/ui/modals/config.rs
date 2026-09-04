use super::{FilterableModal, fuzzy_matches};

/// /config 模态弹窗中的设置项类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigItemKind {
    Model,
    Variant,
    WebSearch,
    ThinkingDisplay,
}

/// /config 模态弹窗中的单个设置项
#[derive(Debug, Clone)]
pub struct ConfigSettingItem {
    pub kind: ConfigItemKind,
    pub name: String,
    pub current_value: String,
    pub description: String,
}

/// /config 模态弹窗状态机
#[derive(Debug, Clone)]
pub struct ConfigModalState {
    pub items: Vec<ConfigSettingItem>,
    pub filtered_items: Vec<ConfigSettingItem>,
    pub selected_index: usize,
    pub query: String,
}

impl ConfigModalState {
    pub fn new(
        current_model: String,
        current_variant: Option<String>,
        web_search: bool,
        thinking_concise: bool,
    ) -> Self {
        let items = vec![
            ConfigSettingItem {
                kind: ConfigItemKind::Model,
                name: "Active Model".to_string(),
                current_value: if current_model.is_empty() {
                    "default".into()
                } else {
                    current_model
                },
                description: "Select target model from configured providers".to_string(),
            },
            ConfigSettingItem {
                kind: ConfigItemKind::Variant,
                name: "Reasoning Effort".to_string(),
                current_value: current_variant.unwrap_or_else(|| "none".into()),
                description: "Select reasoning variant / thinking effort level".to_string(),
            },
            ConfigSettingItem {
                kind: ConfigItemKind::WebSearch,
                name: "Web Search Grounding".to_string(),
                current_value: if web_search {
                    "Enabled".into()
                } else {
                    "Disabled".into()
                },
                description: "Toggle native search grounding for models".to_string(),
            },
            ConfigSettingItem {
                kind: ConfigItemKind::ThinkingDisplay,
                name: "Thinking Display".to_string(),
                current_value: if thinking_concise {
                    "Concise (1 line)".into()
                } else {
                    "Full".into()
                },
                description: "Toggle thinking view (/thinking): concise vs full".to_string(),
            },
        ];

        let mut state = Self {
            items,
            filtered_items: Vec::new(),
            selected_index: 0,
            query: String::new(),
        };
        state.apply_filter();
        state
    }

    pub fn apply_filter(&mut self) {
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| {
                self.query.is_empty()
                    || fuzzy_matches(&self.query, &item.name)
                    || fuzzy_matches(&self.query, &item.current_value)
                    || fuzzy_matches(&self.query, &item.description)
            })
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    /// 更新指定设置项的展示值，并保持光标停留在该项上
    pub fn update_item_value(&mut self, kind: ConfigItemKind, value: String) {
        let selected_kind = self.selected_item().map(|item| item.kind);
        if let Some(item) = self.items.iter_mut().find(|it| it.kind == kind) {
            item.current_value = value;
        }
        self.apply_filter();
        if let Some(kind) = selected_kind {
            self.selected_index = self
                .filtered_items
                .iter()
                .position(|item| item.kind == kind)
                .unwrap_or(0);
        }
    }

    pub fn selected_item(&self) -> Option<&ConfigSettingItem> {
        self.filtered_items.get(self.selected_index)
    }
}

impl FilterableModal for ConfigModalState {
    fn filtered_len(&self) -> usize {
        self.filtered_items.len()
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
    fn test_config_modal_toggle_restores_selection() {
        let mut cm =
            ConfigModalState::new("test_model".into(), Some("medium".into()), false, false);

        // 导航至 WebSearch 项并切换取值
        cm.next();
        cm.next();
        assert_eq!(cm.selected_index, 2);
        cm.update_item_value(
            ConfigItemKind::WebSearch,
            "Enabled".to_string(),
        );
        assert_eq!(cm.items[2].current_value, "Enabled");

        // toggle 内部会重建过滤列表，选中项应被恢复而非重置，后续导航仍指向下一项
        cm.next();
        assert_eq!(cm.selected_index, 3);
        cm.update_item_value(
            ConfigItemKind::ThinkingDisplay,
            "Concise (1 line)".to_string(),
        );
        assert_eq!(cm.items[3].current_value, "Concise (1 line)");
    }
}
