pub mod config;
pub mod models;
pub mod sessions;
pub mod variants;

pub use config::{ConfigItemKind, ConfigModalState, ConfigSettingItem};
pub use models::{ModelItem, ModelsModalState};
pub use sessions::{ProjectScope, SessionsModalState};
pub use variants::{VariantItem, VariantsModalState};

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
