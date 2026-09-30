//! Web 界面：Axum 服务器提供 REST/SSE API 与内嵌前端静态资源。

pub mod api;
mod find;
pub mod server;
mod sync;

pub use server::run_server;
