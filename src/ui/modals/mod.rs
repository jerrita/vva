pub mod config;
pub mod models;
pub mod sessions;
pub mod variants;

pub use config::{ConfigItemKind, ConfigModalState, ConfigSettingItem};
pub use models::{ModelItem, ModelsModalState};
pub use sessions::{ProjectScope, SessionsModalState};
pub use variants::{VariantItem, VariantsModalState};

pub(crate) fn fuzzy_matches(query: &str, candidate: &str) -> bool {
    let candidate = candidate.to_lowercase();
    let mut candidate = candidate.chars();
    query
        .to_lowercase()
        .chars()
        .all(|query_char| candidate.by_ref().any(|c| c == query_char))
}

/// 当前活跃的弹窗视图类型
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ActiveModal {
    #[default]
    None,
    Sessions,
    Models,
    Variants,
    Config,
    Help,
}
