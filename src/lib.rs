// 第二步迁移期间保留应用调用路径；第四步移除。
pub use vva_core::{event, provider, tools};
pub mod model {
    pub use crate::app::model::{Session, SessionHeader, SessionSummary};
    pub use crate::app::records::{Origin, SessionKind, SessionToggles, ToggleOverrides};
    pub use vva_core::model::*;
}
pub mod storage {
    pub use crate::app::store::AppSessionStore as SessionStore;
    pub use vva_core::storage::{StoreError, project_key};
    pub mod jsonl {
        pub use crate::app::store::AppSessionStore as JsonlSessionStore;
    }
}
pub mod harness {
    pub use crate::app::handle::ManagedSession;
    pub use crate::app::view::SessionView;
    pub use vva_core::harness::*;
}
pub mod agents;
pub mod app;
pub mod config;
pub mod discover;
pub mod find;
pub mod skills;
pub mod space;
pub mod system;
pub mod ui;
pub mod web;

#[cfg(test)]
mod test_support;
