//! Web API：以 REST + SSE 暴露 Harness 会话能力，前端作为唯一数据源消费。

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::SessionView;
use crate::ui::history::compute_stats;

use super::server::AppState;

fn error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": message.to_string() })),
    )
        .into_response()
}

fn not_found(id: &str) -> Response {
    error(StatusCode::NOT_FOUND, format!("Session not found: {id}"))
}

/// 会话快照：已完成轮次 + 流式草稿 + 运行状态，前端以 revision 判定更新
#[derive(Serialize)]
struct SessionSnapshot {
    revision: u64,
    state: String,
    persisted: bool,
    notices: Vec<String>,
    session: Value,
    draft: Value,
}

impl From<&SessionView> for SessionSnapshot {
    fn from(view: &SessionView) -> Self {
        Self {
            revision: view.revision,
            state: view.state.status(),
            persisted: view.persisted,
            notices: view.notices.clone(),
            session: serde_json::to_value(&*view.session).unwrap_or(Value::Null),
            draft: serde_json::to_value(&view.draft).unwrap_or(Value::Null),
        }
    }
}

#[derive(Serialize)]
struct ModelEntry {
    spec: String,
    name: String,
    provider: String,
    variants: Vec<String>,
}

#[derive(Serialize)]
struct ModelsResponse {
    default: String,
    models: Vec<ModelEntry>,
}

/// 列出配置中的全部模型规格，供前端模型选择器使用
pub async fn list_models(State(state): State<AppState>) -> Response {
    let config = state.manager.app_config();
    let mut models = Vec::new();
    for (provider_id, provider) in &config.providers {
        for (model_id, model) in &provider.models {
            models.push(ModelEntry {
                spec: format!("{provider_id}/{model_id}"),
                name: model.name.clone(),
                provider: provider_id.clone(),
                variants: model.variants.clone().unwrap_or_default(),
            });
        }
    }
    models.sort_by(|a, b| a.spec.cmp(&b.spec));
    Json(ModelsResponse {
        default: config.settings.default.clone(),
        models,
    })
    .into_response()
}

pub async fn list_sessions(State(state): State<AppState>) -> Response {
    match state.manager.store().list_sessions().await {
        Ok(sessions) => Json(sessions).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// 服务器默认工作空间，供新建会话的项目选择器使用
pub async fn default_workspace(State(state): State<AppState>) -> Response {
    Json(serde_json::json!({
        "root": state.manager.workspace().to_string_lossy()
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct DirQuery {
    pub path: Option<String>,
}

/// 列出目录下的一级子目录，供新建项目多级点选
pub async fn list_dirs(State(state): State<AppState>, Query(query): Query<DirQuery>) -> Response {
    let path = match query.path.as_deref().map(str::trim) {
        Some(path) if !path.is_empty() => std::path::PathBuf::from(path),
        _ => state.manager.workspace().to_path_buf(),
    };
    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_dir() => {}
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("目录不存在: {}", path.display()),
            );
        }
    }
    let mut dirs = Vec::new();
    let mut entries = match tokio::fs::read_dir(&path).await {
        Ok(entries) => entries,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let is_dir = tokio::fs::metadata(entry.path())
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if is_dir {
            dirs.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    dirs.sort();
    let parent = path
        .parent()
        .map(|parent| parent.to_string_lossy().to_string());
    Json(serde_json::json!({
        "root": path.to_string_lossy(),
        "parent": parent,
        "dirs": dirs,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub model: Option<String>,
    pub variant: Option<String>,
    pub search: Option<bool>,
    /// 工作空间目录；缺省为服务器启动目录
    pub workspace: Option<String>,
}

pub async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Response {
    let model = request
        .model
        .clone()
        .unwrap_or_else(|| state.manager.app_config().settings.default.clone());
    let result = match request.workspace.as_deref().map(str::trim) {
        Some(workspace) if !workspace.is_empty() => {
            state
                .manager
                .create_session_in(
                    workspace,
                    model,
                    request.variant,
                    None,
                    request.search.unwrap_or(true),
                )
                .await
        }
        _ => {
            state
                .manager
                .create_session(model, request.variant, None, request.search.unwrap_or(true))
                .await
        }
    };
    match result {
        Ok(handle) => Json(SessionSnapshot::from(&*handle.snapshot())).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

pub async fn get_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.manager.get_or_load(&id).await {
        Ok(handle) => Json(SessionSnapshot::from(&*handle.snapshot())).into_response(),
        Err(_) => not_found(&id),
    }
}

pub async fn delete_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.manager.get_or_load(&id).await {
        Ok(handle) => match state.manager.delete_session(&handle).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        Err(_) => not_found(&id),
    }
}

#[derive(Deserialize)]
pub struct SubmitMessageRequest {
    pub prompt: String,
}

pub async fn submit_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SubmitMessageRequest>,
) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    match handle.submit(&request.prompt, None).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        // 会话正在执行其他请求时拒绝重复提交
        Err(e) => error(StatusCode::CONFLICT, e),
    }
}

#[derive(Deserialize)]
pub struct RevertRequest {
    pub turn_index: usize,
}

/// 回退到指定用户轮次：丢弃该轮次及其后的全部内容
pub async fn revert_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<RevertRequest>,
) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    match handle.revert(request.turn_index).await {
        Ok(()) => Json(SessionSnapshot::from(&*handle.snapshot())).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(Deserialize)]
pub struct ForkRequest {
    pub turn_index: usize,
    /// 新会话标题；缺省时由分叉点用户消息派生
    pub title: Option<String>,
}

/// 从指定用户轮次分叉新会话：保留该轮次之前的历史，原会话不变
pub async fn fork_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ForkRequest>,
) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    match state
        .manager
        .fork_session(
            &handle,
            request.turn_index,
            request.title.unwrap_or_default(),
        )
        .await
    {
        Ok(created) => Json(SessionSnapshot::from(&*created.snapshot())).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

pub async fn abort_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    match handle.abort_current().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(StatusCode::CONFLICT, e),
    }
}

/// serde 双层 Option：区分字段缺席（保持不变）与显式 null（清空档位）
mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Ok(Some(Option::<T>::deserialize(deserializer)?))
    }
}

#[derive(Deserialize)]
pub struct ConfigureSessionRequest {
    pub model: Option<String>,
    #[serde(default, deserialize_with = "double_option::deserialize")]
    pub variant: Option<Option<String>>,
    pub search: Option<bool>,
}

pub async fn configure_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ConfigureSessionRequest>,
) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    match handle
        .configure(request.model, request.variant, request.search)
        .await
    {
        Ok(()) => Json(SessionSnapshot::from(&*handle.snapshot())).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// 会话统计 (Token 消耗、缓存命中与预估花费)，复用 TUI 侧栏的 compute_stats 投影
pub async fn session_stats(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    let view = handle.snapshot();
    Json(compute_stats(&view.session, state.manager.app_config())).into_response()
}

/// SSE 订阅会话视图；连接建立即推送当前快照，此后按发布节流推送增量
pub async fn stream_events(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };

    let rx = handle.subscribe();
    let pending = Some(handle.snapshot());
    let stream: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> = Box::pin(
        futures::stream::unfold((rx, pending), |(mut rx, mut pending)| async move {
            if let Some(view) = pending.take() {
                return Some((Ok(sse_event(&view)), (rx, None)));
            }
            match rx.changed().await {
                Ok(()) => {
                    let view = rx.borrow_and_update().clone();
                    Some((Ok(sse_event(&view)), (rx, None)))
                }
                Err(_) => None,
            }
        }),
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_event(view: &SessionView) -> Event {
    Event::default().data(serde_json::to_string(&SessionSnapshot::from(view)).unwrap_or_default())
}
