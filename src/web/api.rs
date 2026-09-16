//! Web API：以 REST + SSE 暴露 Harness 会话能力，前端作为唯一数据源消费。

use std::convert::Infallible;
use std::path::Path as StdPath;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use crate::ui::history::compute_stats;

use super::server::AppState;
use super::sync::{SessionDelta, SessionSnapshot};

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

#[derive(Serialize)]
struct SkillEntry {
    name: String,
    description: String,
    source: &'static str,
    path: String,
}

#[derive(Serialize)]
struct SkillsResponse {
    skills: Vec<SkillEntry>,
}

/// 列出已发现的 Agent Skills（全局 + 项目，项目覆盖同名），供前端只读展示
pub async fn list_skills(State(state): State<AppState>) -> Response {
    let workspace = state.manager.workspace().to_path_buf();
    let skills = tokio::task::spawn_blocking(move || crate::skills::discover_skills(&workspace))
        .await
        .unwrap_or_default();
    Json(SkillsResponse {
        skills: skills
            .into_iter()
            .map(|skill| SkillEntry {
                name: skill.name,
                description: skill.description,
                source: match skill.source {
                    crate::skills::SkillSource::Global => "global",
                    crate::skills::SkillSource::Project => "project",
                },
                path: skill.path.to_string_lossy().into_owned(),
            })
            .collect(),
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
pub struct FsEntriesQuery {
    pub workspace: Option<String>,
    pub path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FsEntriesResponse {
    pub workspace: String,
    pub path: String,
    pub entries: Vec<FsEntry>,
}

/// 列出工程目录下的条目（目录与文件），支持工程文件浏览器懒加载
pub async fn list_fs_entries(
    State(state): State<AppState>,
    Query(query): Query<FsEntriesQuery>,
) -> Response {
    let workspace = match query.workspace.as_deref().map(str::trim) {
        Some(ws) if !ws.is_empty() => std::path::PathBuf::from(ws),
        _ => state.manager.workspace().to_path_buf(),
    };

    let subpath = query.path.as_deref().unwrap_or("").trim();
    let target_dir = crate::tools::filesystem::resolve_path(&workspace, subpath);

    match tokio::fs::metadata(&target_dir).await {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return error(StatusCode::BAD_REQUEST, "目标路径不是目录"),
        Err(e) => return error(StatusCode::NOT_FOUND, format!("目录不存在: {e}")),
    }

    let mut read_dir = match tokio::fs::read_dir(&target_dir).await {
        Ok(entries) => entries,
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("读取目录失败: {e}"),
            );
        }
    };

    let mut entries = Vec::new();
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let file_name = entry.file_name().to_string_lossy().to_string();
        // 过滤内部版本控制系统目录
        if file_name == ".git" {
            continue;
        }

        let entry_path = entry.path();
        let meta = match tokio::fs::metadata(&entry_path).await {
            Ok(m) => m,
            Err(_) => continue,
        };

        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };

        let rel_path = match entry_path.strip_prefix(&workspace) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => entry_path.to_string_lossy().replace('\\', "/"),
        };

        entries.push(FsEntry {
            name: file_name,
            path: rel_path,
            is_dir,
            size,
        });
    }

    // 排序：目录优先，同类按名称不区分大小写排序
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    let rel_target = match target_dir.strip_prefix(&workspace) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => String::new(),
    };

    Json(FsEntriesResponse {
        workspace: workspace.to_string_lossy().to_string(),
        path: rel_target,
        entries,
    })
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
pub struct SessionFileQuery {
    pub path: String,
}

#[derive(Deserialize)]
pub struct FsFileQuery {
    pub workspace: Option<String>,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SessionFileResponse {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_binary: bool,
    pub content: String,
    pub mime_type: Option<String>,
}

/// 读取指定工作区文件的实时内容，供无会话或直连场景预览
pub async fn get_fs_file(
    State(state): State<AppState>,
    Query(query): Query<FsFileQuery>,
) -> Response {
    let workspace = match query.workspace.as_deref().map(str::trim) {
        Some(ws) if !ws.is_empty() => std::path::PathBuf::from(ws),
        _ => state.manager.workspace().to_path_buf(),
    };
    read_file_from_workspace(&workspace, &query.path).await
}

/// 读取会话相关文件的实时内容，供前端侧栏预览
pub async fn get_session_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<SessionFileQuery>,
) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    let workspace = std::path::PathBuf::from(&handle.snapshot().session.workspace_root);
    read_file_from_workspace(&workspace, &query.path).await
}

/// 下载指定工作区文件，保留原始二进制内容
pub async fn download_fs_file(
    State(state): State<AppState>,
    Query(query): Query<FsFileQuery>,
) -> Response {
    let workspace = match query.workspace.as_deref().map(str::trim) {
        Some(ws) if !ws.is_empty() => std::path::PathBuf::from(ws),
        _ => state.manager.workspace().to_path_buf(),
    };
    download_file_from_workspace(&workspace, &query.path).await
}

/// 下载会话工作区文件，保留原始二进制内容
pub async fn download_session_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<SessionFileQuery>,
) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    let workspace = std::path::PathBuf::from(&handle.snapshot().session.workspace_root);
    download_file_from_workspace(&workspace, &query.path).await
}

async fn download_file_from_workspace(workspace: &std::path::Path, rel_path: &str) -> Response {
    let path = crate::tools::filesystem::resolve_path(workspace, rel_path);
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta,
        Err(e) => return error(StatusCode::NOT_FOUND, format!("文件不存在: {e}")),
    };
    if metadata.is_dir() {
        return error(StatusCode::BAD_REQUEST, "目标是目录而非文件");
    }

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("读取文件失败: {e}"),
            );
        }
    };
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
        .replace(['"', '\\', '\r', '\n'], "_");
    let encoded_name = name
        .as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (*byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect::<String>();
    let mime = mime_guess::from_path(&path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"download\"; filename*=UTF-8''{encoded_name}"
    )) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// 从指定工作区读取文件内容并返回统一的 SessionFileResponse 响应
pub async fn read_file_from_workspace(workspace: &std::path::Path, rel_path: &str) -> Response {
    let path = crate::tools::filesystem::resolve_path(workspace, rel_path);

    let metadata = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta,
        Err(e) => return error(StatusCode::NOT_FOUND, format!("文件不存在: {e}")),
    };

    if metadata.is_dir() {
        return error(StatusCode::BAD_REQUEST, "目标是目录而非文件");
    }

    let size = metadata.len();
    if size > 10 * 1024 * 1024 {
        return error(
            StatusCode::BAD_REQUEST,
            "文件过大，不支持在线预览 (超过 10MB)",
        );
    }

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("读取文件失败: {e}"),
            );
        }
    };

    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let path_str = path.to_string_lossy().into_owned();

    // 检查是否为有效 UTF-8 文本
    if let Ok(text) = std::str::from_utf8(&bytes)
        && !text.contains('\0')
    {
        let mime_type = mime_guess::from_path(&path).first().map(|m| m.to_string());
        return Json(SessionFileResponse {
            path: path_str,
            name,
            size,
            is_binary: false,
            content: text.to_string(),
            mime_type,
        })
        .into_response();
    }

    // 二进制媒体检测（图片等）
    let mime_type = mime_guess::from_path(&path).first().map(|m| m.to_string());
    let is_image = mime_type
        .as_deref()
        .map(|m| m.starts_with("image/"))
        .unwrap_or(false);

    if is_image {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Json(SessionFileResponse {
            path: path_str,
            name,
            size,
            is_binary: true,
            content: b64,
            mime_type,
        })
        .into_response();
    }

    Json(SessionFileResponse {
        path: path_str,
        name,
        size,
        is_binary: true,
        content: String::new(),
        mime_type,
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct SubmitMessageRequest {
    pub prompt: String,
    /// 随提示词提交的原生文件附件（Base64 编码）
    #[serde(default)]
    pub files: Vec<SubmitFile>,
}

#[derive(Deserialize)]
pub struct SubmitFile {
    pub filename: String,
    pub mime_type: String,
    pub data: String,
}

#[derive(Deserialize)]
pub struct UploadFileRequest {
    pub filename: String,
    pub data: String,
    pub workspace: Option<String>,
}

/// 将浏览器无法原生传给模型的文件保存到当前工作区 scratch。
pub async fn upload_file(
    State(state): State<AppState>,
    Json(request): Json<UploadFileRequest>,
) -> Response {
    let filename = StdPath::new(&request.filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("upload");
    let bytes = match base64::engine::general_purpose::STANDARD.decode(&request.data) {
        Ok(bytes) => bytes,
        Err(decode_error) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("Invalid file data: {decode_error}"),
            );
        }
    };
    let workspace = request
        .workspace
        .as_deref()
        .map(StdPath::new)
        .unwrap_or_else(|| state.manager.workspace());
    let scratch = match state.manager.store().ensure_scratch(workspace).await {
        Ok(path) => path,
        Err(store_error) => return error(StatusCode::INTERNAL_SERVER_ERROR, store_error),
    };
    let path = scratch.join(format!("{}_{}", uuid::Uuid::new_v4(), filename));
    if let Err(io_error) = tokio::fs::write(&path, bytes).await {
        return error(StatusCode::INTERNAL_SERVER_ERROR, io_error);
    }
    Json(serde_json::json!({"path": path})).into_response()
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
    let files = request
        .files
        .into_iter()
        .map(|file| match file.mime_type.as_str() {
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" => {
                crate::model::ContentPart::Image {
                    source: crate::model::MediaSource::Base64 {
                        data: file.data,
                        mime_type: file.mime_type,
                    },
                    mime_type: None,
                }
            }
            _ => crate::model::ContentPart::File {
                source: crate::model::MediaSource::Base64 {
                    data: file.data,
                    mime_type: file.mime_type.clone(),
                },
                mime_type: file.mime_type,
                filename: file.filename,
            },
        })
        .collect();
    match handle.submit(&request.prompt, files, None).await {
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

#[derive(Deserialize)]
pub struct AnswerUserRequest {
    pub request_id: String,
    pub answers: Vec<crate::model::UserAnswer>,
}

pub async fn answer_user(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AnswerUserRequest>,
) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };
    match handle
        .answer_user(request.request_id, request.answers)
        .await
    {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(message) => error(StatusCode::BAD_REQUEST, message),
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
    Json(compute_stats(&view.session, &state.manager.app_config())).into_response()
}

/// 全局配置文件寻址：`~/.vva/config.jsonc` 优先，其次 `config.json`，
/// 两者都不存在时以 `config.json` 作为写入目标
fn global_config_path() -> Option<std::path::PathBuf> {
    let dir = crate::config::home_dir()?.join(".vva");
    let jsonc = dir.join("config.jsonc");
    if jsonc.exists() {
        return Some(jsonc);
    }
    let json = dir.join("config.json");
    if json.exists() {
        return Some(json);
    }
    Some(dir.join("config.json"))
}

/// 读取全局配置文件原文（JSONC），供 Settings 弹窗编辑；文件不存在时内容为 null
pub async fn get_global_config() -> Response {
    let Some(path) = global_config_path() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "无法定位用户主目录 (~)");
    };
    let content = tokio::fs::read_to_string(&path).await.ok();
    Json(serde_json::json!({
        "path": path.display().to_string(),
        "content": content,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct UpdateConfigRequest {
    pub content: String,
}

/// 保存全局配置：先按完整解析流程校验，落盘后热重载合并配置供后续会话使用
pub async fn update_global_config(
    State(state): State<AppState>,
    Json(request): Json<UpdateConfigRequest>,
) -> Response {
    let Some(path) = global_config_path() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "无法定位用户主目录 (~)");
    };
    if let Err(e) = crate::config::parse_config_str(&request.content) {
        return error(StatusCode::BAD_REQUEST, e);
    }
    if let Some(parent) = path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    if let Err(e) = tokio::fs::write(&path, &request.content).await {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    if let Err(e) = state.manager.reload_config() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("配置已保存，但热重载失败: {e}"),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

/// 每条连接以快照建立基线，再发送相对已发送版本的增量。
/// watch 可合并中间发布；慢客户端仍从自己的基线更新到最新状态。
pub async fn stream_events(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let handle = match state.manager.get_or_load(&id).await {
        Ok(handle) => handle,
        Err(_) => return not_found(&id),
    };

    let mut rx = handle.subscribe();
    let initial = rx.borrow_and_update().clone();
    let stream: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(futures::stream::unfold(
            (rx, initial, true),
            |(mut rx, previous, first)| async move {
                if first {
                    let event = Event::default()
                        .event("snapshot")
                        .json_data(SessionSnapshot::from(&*previous))
                        .expect("session snapshot is serializable");
                    return Some((Ok(event), (rx, previous, false)));
                }
                if rx.changed().await.is_err() {
                    return None;
                }
                let current = rx.borrow_and_update().clone();
                let event = Event::default()
                    .event("delta")
                    .json_data(SessionDelta::between(&previous, &current))
                    .expect("session delta is serializable");
                Some((Ok(event), (rx, current, false)))
            },
        ));
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::SessionManager;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_list_fs_entries_filters_git_and_sorts() {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();

        tokio::fs::create_dir_all(ws.join(".git")).await.unwrap();
        tokio::fs::write(ws.join(".git/config"), "dummy")
            .await
            .unwrap();

        tokio::fs::create_dir_all(ws.join("src/nested"))
            .await
            .unwrap();
        tokio::fs::write(ws.join("src/lib.rs"), "pub fn test() {}")
            .await
            .unwrap();
        tokio::fs::write(ws.join("Cargo.toml"), "[package]")
            .await
            .unwrap();
        tokio::fs::write(ws.join("README.md"), "# Test")
            .await
            .unwrap();

        let temp_sessions = TempDir::new().unwrap();
        let store =
            Arc::new(crate::storage::jsonl::JsonlSessionStore::new(temp_sessions.path()).unwrap());
        let config = crate::config::AppConfig::default();
        let manager = Arc::new(SessionManager::new(
            store,
            config,
            ws,
            ws.join("state.json"),
        ));
        let state = AppState { manager };

        let response = list_fs_entries(
            State(state.clone()),
            Query(FsEntriesQuery {
                workspace: Some(ws.to_string_lossy().to_string()),
                path: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let res: FsEntriesResponse = serde_json::from_slice(&body).unwrap();

        assert!(!res.entries.iter().any(|e| e.name == ".git"));
        assert_eq!(res.entries[0].name, "src");
        assert!(res.entries[0].is_dir);
        assert_eq!(res.entries[0].path, "src");

        let file_names: Vec<&str> = res.entries[1..].iter().map(|e| e.name.as_str()).collect();
        assert_eq!(file_names, vec!["Cargo.toml", "README.md"]);

        let sub_response = list_fs_entries(
            State(state.clone()),
            Query(FsEntriesQuery {
                workspace: Some(ws.to_string_lossy().to_string()),
                path: Some("src".into()),
            }),
        )
        .await;

        assert_eq!(sub_response.status(), StatusCode::OK);
        let sub_body = axum::body::to_bytes(sub_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let sub_res: FsEntriesResponse = serde_json::from_slice(&sub_body).unwrap();
        assert_eq!(sub_res.entries[0].name, "nested");
        assert!(sub_res.entries[0].is_dir);
        assert_eq!(sub_res.entries[0].path, "src/nested");
        assert_eq!(sub_res.entries[1].name, "lib.rs");
        assert!(!sub_res.entries[1].is_dir);
        assert_eq!(sub_res.entries[1].path, "src/lib.rs");
    }

    #[tokio::test]
    async fn test_read_file_from_workspace() {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();
        tokio::fs::write(ws.join("hello.txt"), "hello world")
            .await
            .unwrap();

        let response = read_file_from_workspace(ws, "hello.txt").await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let res: SessionFileResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(res.name, "hello.txt");
        assert_eq!(res.content, "hello world");
        assert!(!res.is_binary);

        let missing = read_file_from_workspace(ws, "nonexistent.txt").await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}
