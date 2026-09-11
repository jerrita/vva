use axum::{
    Router,
    routing::{get, post},
    Extension,
    response::IntoResponse,
    http::{header, Uri, StatusCode},
};
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::config::AppConfig;
use crate::harness::SessionManager;
use crate::storage::SessionStore;

use super::api;

#[derive(RustEmbed)]
#[folder = "frontend/dist"]
struct Asset;

#[derive(Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub store: Arc<dyn SessionStore + Send + Sync>,
    pub config: AppConfig,
}

pub async fn run_server(
    store: Arc<dyn SessionStore + Send + Sync>,
    config: AppConfig,
    manager: Arc<SessionManager>,
    port: u16,
    host: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        session_manager: manager,
        store,
        config,
    };

    let app = Router::new()
        .route("/api/sessions", get(api::list_sessions).post(api::create_session))
        .route("/api/sessions/{id}", get(api::get_session))
        .route("/api/sessions/{id}/prompt", post(api::submit_prompt))
        .route("/api/sessions/{id}/stream", get(api::stream_events))
        .fallback(static_handler)
        .layer(CorsLayer::permissive())
        .layer(Extension(state));

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
    info!("Starting web server at http://{}", addr);
    println!("Starting web server at http://{}", addr);

    // Try to open the browser if available
    let url = format!("http://{}", addr);
    let _ = open::that(&url);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let mut path = uri.path().trim_start_matches('/').to_string();

    if path.is_empty() {
        path = "index.html".to_string();
    }

    match Asset::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response()
        }
        None => {
            // Fallback to index.html for client-side routing
            if let Some(index) = Asset::get("index.html") {
                ([(header::CONTENT_TYPE, "text/html")], index.data).into_response()
            } else {
                (StatusCode::NOT_FOUND, "404 Not Found").into_response()
            }
        }
    }
}
