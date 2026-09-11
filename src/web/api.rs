use axum::{
    extract::{Path, Extension},
    response::{IntoResponse, sse::{Event, Sse}},
    Json,
};
use serde::{Deserialize, Serialize};
use core::convert::Infallible;

use super::server::AppState;

#[derive(Serialize)]
pub struct ErrorResponse {
    error: String,
}

pub async fn list_sessions(
    Extension(state): Extension<AppState>,
) -> impl IntoResponse {
    match state.store.list_sessions().await {
        Ok(sessions) => Json(sessions).into_response(),
        Err(e) => (
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e.to_string() }),
        ).into_response(),
    }
}

pub async fn get_session(
    Path(id): Path<String>,
    Extension(state): Extension<AppState>,
) -> impl IntoResponse {
    match state.store.load_session(&id).await {
        Ok(Some(session)) => Json(session).into_response(),
        Ok(None) => (
            reqwest::StatusCode::NOT_FOUND,
            Json(ErrorResponse { error: "Session not found".into() }),
        ).into_response(),
        Err(e) => (
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e.to_string() }),
        ).into_response(),
    }
}

#[derive(Deserialize)]
pub struct CreateSessionReq {
    pub workspace_root: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub title: Option<String>,
    pub search: Option<bool>,
}

pub async fn create_session(
    Extension(state): Extension<AppState>,
    Json(req): Json<CreateSessionReq>,
) -> impl IntoResponse {
    let _workspace = req.workspace_root
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    let default_model = state.config.settings.default.clone();
    let model = req.model.unwrap_or(default_model);

    match state.session_manager.create_session(model, req.variant, req.title, req.search.unwrap_or(false)).await {
        Ok(session) => Json(session.snapshot().session.id.clone()).into_response(),
        Err(e) => (
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e }),
        ).into_response(),
    }
}

#[derive(Deserialize)]
pub struct SubmitPromptReq {
    pub prompt: String,
}

pub async fn submit_prompt(
    Path(id): Path<String>,
    Extension(state): Extension<AppState>,
    Json(req): Json<SubmitPromptReq>,
) -> impl IntoResponse {
    let session = match state.session_manager.open_session(Some(&id), None, None, None).await {
        Ok(s) => s,
        Err(_) => {
            return (
                reqwest::StatusCode::NOT_FOUND,
                Json(ErrorResponse { error: "Session not found".into() }),
            ).into_response();
        }
    };

    match session.submit(&req.prompt, None).await {
        Ok(_) => Json(serde_json::json!({ "status": "ok" })).into_response(),
        Err(e) => (
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e }),
        ).into_response(),
    }
}

pub async fn stream_events(
    Path(id): Path<String>,
    Extension(state): Extension<AppState>,
) -> impl IntoResponse {
    let session = match state.session_manager.open_session(Some(&id), None, None, None).await {
        Ok(s) => s,
        Err(_) => {
            return (
                reqwest::StatusCode::NOT_FOUND,
                Json(ErrorResponse { error: "Session not found".into() }),
            ).into_response();
        }
    };

    let mut rx = session.subscribe();

    let stream = async_stream::stream! {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let view = rx.borrow().clone();

            // For SSE we can send JSON representations
            let data = serde_json::json!({
                "state": view.state.status(),
                "draft": view.draft,
                "notices": view.notices,
                "revision": view.revision,
            }).to_string();

            yield Ok::<_, Infallible>(Event::default().data(data));
        }
    };

    Sse::new(stream).into_response()
}
