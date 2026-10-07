//! Axum 服务器组装：API 路由与前端静态资源（SPA 回退）。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, serve};
use rust_embed::RustEmbed;

use crate::harness::SessionManager;

use super::api;

/// 前端构建产物在构件期由 build.rs 编译产出，rust_embed 打包进二进制
#[derive(RustEmbed)]
#[folder = "frontend/dist"]
struct Assets;

#[derive(Clone)]
pub struct AppState {
    pub manager: Arc<SessionManager>,
}

pub async fn run_server(
    manager: Arc<SessionManager>,
    host: String,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new()
        .route("/api/models", get(api::list_models))
        .route("/api/skills", get(api::list_skills))
        .route("/api/workspace", get(api::default_workspace))
        .route("/api/fs/dirs", get(api::list_dirs))
        .route("/api/fs/entries", get(api::list_fs_entries))
        .route("/api/fs/find", get(api::find_fs_files))
        .route("/api/fs/file", get(api::get_fs_file))
        .route("/api/fs/download", get(api::download_fs_file))
        // 上传文件不设请求体大小上限，包括 Base64 编码后的体积。
        .route(
            "/api/files",
            post(api::upload_file).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/config",
            get(api::get_global_config).post(api::update_global_config),
        )
        .route(
            "/api/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route(
            "/api/sessions/{id}",
            get(api::get_session).delete(api::delete_session),
        )
        .route("/api/sessions/{id}/file", get(api::get_session_file))
        .route(
            "/api/sessions/{id}/download",
            get(api::download_session_file),
        )
        // 图片和 PDF 附件随消息提交，使用同样的上传策略。
        .route(
            "/api/sessions/{id}/messages",
            post(api::submit_message).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/sessions/{id}/revert", post(api::revert_session))
        .route("/api/sessions/{id}/fork", post(api::fork_session))
        .route("/api/sessions/{id}/abort", post(api::abort_session))
        .route(
            "/api/sessions/{id}/notices/dismiss",
            post(api::dismiss_notice),
        )
        .route("/api/sessions/{id}/answer", post(api::answer_user))
        .route("/api/sessions/{id}/config", post(api::configure_session))
        .route("/api/sessions/{id}/events", get(api::stream_events))
        .route("/api/sessions/{id}/stats", get(api::session_stats))
        .fallback(static_handler)
        .with_state(AppState { manager });

    let addr = SocketAddr::new(host.parse()?, port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("🌐 [vva] Web 界面已启动: http://{addr}");
    serve(listener, app).await?;
    Ok(())
}

/// 静态资源服务；未命中路径回退 index.html 以支持前端路由
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(asset) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            [(header::CONTENT_TYPE, mime.as_ref().to_owned())],
            asset.data,
        )
            .into_response();
    }

    match Assets::get("index.html") {
        Some(index) => ([(header::CONTENT_TYPE, "text/html")], index.data).into_response(),
        None => (StatusCode::NOT_FOUND, "frontend/dist is missing").into_response(),
    }
}
