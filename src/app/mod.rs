//! vva 应用策略：配置、coding 上下文和会话装配。
pub(crate) mod mention;
pub mod session;
pub mod setup;
mod web;

pub use session::VvaSessionManager;
pub use setup::{SessionPreamble, SessionToolFactory, VvaSessionSetup};

#[cfg(test)]
mod session_tests;

pub mod handle;
pub mod migration;
pub mod model;
pub mod records;
pub mod store;
pub mod view;

pub mod stats;
