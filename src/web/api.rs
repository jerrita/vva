//! Web API：以 REST + SSE 暴露 Harness 会话能力，前端作为唯一数据源消费。

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::Display;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use crate::harness::ManagedSession;
use crate::model::{ContentPart, MediaSource, SessionSummary, UserAnswer};
use crate::ui::history::{SidebarStats, compute_stats};

use super::server::AppState;
use super::sync::{SessionDelta, SessionSnapshot};

/// 统一错误响应：`{ "error": message }` + 状态码
pub struct ApiError(StatusCode, String);

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

/// `map_err` 适配器：以给定状态码包装任意可显示错误
fn fail<E: Display>(status: StatusCode) -> impl Fn(E) -> ApiError {
    move |e| ApiError::new(status, e.to_string())
}

type ApiResult<T> = Result<T, ApiError>;

/// 路径 `{id}` 对应的会话句柄提取器；会话不存在时返回 404
pub struct Session(Arc<ManagedSession>);

impl FromRequestParts<AppState> for Session {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let Path(id) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        match state.manager.get_or_load(&id).await {
            Ok(handle) => Ok(Session(handle)),
            Err(_) => Err(
                ApiError::new(StatusCode::NOT_FOUND, format!("Session not found: {id}"))
                    .into_response(),
            ),
        }
    }
}

impl Session {
    fn snapshot_json(&self) -> Response {
        Json(SessionSnapshot::from(&*self.0.snapshot())).into_response()
    }

    fn workspace(&self) -> PathBuf {
        PathBuf::from(&self.0.snapshot().session.header.workspace_root)
    }
}

/// 显式工作区参数（trim 后非空）优先，缺省回退服务器启动目录
fn workspace_or_default(state: &AppState, explicit: Option<&str>) -> PathBuf {
    match explicit.map(str::trim) {
        Some(ws) if !ws.is_empty() => PathBuf::from(ws),
        _ => state.manager.workspace().to_path_buf(),
    }
}

#[derive(Serialize)]
pub struct ModelEntry {
    spec: String,
    name: String,
    provider: String,
    variants: Vec<String>,
}

#[derive(Serialize)]
pub struct ModelsResponse {
    default: String,
    models: Vec<ModelEntry>,
}

/// 列出配置中的全部模型规格，供前端模型选择器使用
pub async fn list_models(State(state): State<AppState>) -> Json<ModelsResponse> {
    let config = state.manager.app_config();
    let mut models: Vec<ModelEntry> = config
        .providers
        .iter()
        .flat_map(|(provider_id, provider)| {
            provider
                .models
                .iter()
                .map(move |(model_id, model)| ModelEntry {
                    spec: format!("{provider_id}/{model_id}"),
                    name: model.name.clone(),
                    provider: provider_id.clone(),
                    variants: model.variants.clone().unwrap_or_default(),
                })
        })
        .collect();
    models.sort_by(|a, b| a.spec.cmp(&b.spec));
    Json(ModelsResponse {
        default: config.settings.default.clone(),
        models,
    })
}

#[derive(Serialize)]
pub struct SkillEntry {
    name: String,
    description: String,
    source: &'static str,
    path: String,
}

#[derive(Serialize)]
pub struct SkillsResponse {
    skills: Vec<SkillEntry>,
}

/// 列出已发现的 Agent Skills（全局 + 项目，项目覆盖同名），供前端只读展示
pub async fn list_skills(State(state): State<AppState>) -> Json<SkillsResponse> {
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
}

#[derive(Serialize)]
pub struct SessionListItem {
    #[serde(flatten)]
    summary: SessionSummary,
    #[serde(skip_serializing_if = "String::is_empty")]
    state: String,
}

fn overlay_session_states(
    sessions: Vec<SessionSummary>,
    live: &HashMap<String, String>,
) -> Vec<SessionListItem> {
    sessions
        .into_iter()
        .map(|summary| {
            let state = live.get(&summary.id).cloned().unwrap_or_default();
            SessionListItem { summary, state }
        })
        .collect()
}

pub async fn list_sessions(State(state): State<AppState>) -> ApiResult<Json<Vec<SessionListItem>>> {
    let sessions = state
        .manager
        .store()
        .list_sessions()
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let live = state.manager.live_run_states().await;
    Ok(Json(overlay_session_states(sessions, &live)))
}

/// 服务器默认工作空间，供新建会话的项目选择器使用
pub async fn default_workspace(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "root": state.manager.workspace().to_string_lossy() }))
}

#[derive(Deserialize)]
pub struct DirQuery {
    pub path: Option<String>,
}

/// 列出目录下的一级子目录，供新建项目多级点选
pub async fn list_dirs(
    State(state): State<AppState>,
    Query(query): Query<DirQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let path = workspace_or_default(&state, query.path.as_deref());
    if !tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_dir()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("目录不存在: {}", path.display()),
        ));
    }
    let mut entries = tokio::fs::read_dir(&path)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let mut dirs = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if tokio::fs::metadata(entry.path())
            .await
            .is_ok_and(|meta| meta.is_dir())
        {
            dirs.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    dirs.sort();
    Ok(Json(serde_json::json!({
        "root": path.to_string_lossy(),
        "parent": path.parent().map(|parent| parent.to_string_lossy()),
        "dirs": dirs,
    })))
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

#[derive(Deserialize)]
pub struct FsFindQuery {
    pub workspace: Option<String>,
    #[serde(default)]
    pub q: String,
    pub limit: Option<usize>,
}

/// 模糊查找命中的文件：`entry` 展平，附带 `path` 中命中的字符下标供前端高亮
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FsFindEntry {
    #[serde(flatten)]
    pub entry: FsEntry,
    pub indices: Vec<usize>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FsFindResponse {
    pub workspace: String,
    pub query: String,
    pub entries: Vec<FsFindEntry>,
    pub truncated: bool,
}

const FIND_DEFAULT_LIMIT: usize = 50;
const FIND_MAX_LIMIT: usize = 200;

/// 工程内文件模糊查找（go to file）；`q` 为空时返回最近修改的文件
pub async fn find_fs_files(
    State(state): State<AppState>,
    Query(query): Query<FsFindQuery>,
) -> ApiResult<Json<FsFindResponse>> {
    let workspace = workspace_or_default(&state, query.workspace.as_deref());
    let limit = query
        .limit
        .unwrap_or(FIND_DEFAULT_LIMIT)
        .clamp(1, FIND_MAX_LIMIT);
    let q = query.q.trim().to_string();

    match tokio::fs::metadata(&workspace).await {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(ApiError::new(StatusCode::BAD_REQUEST, "工作区不是目录")),
        Err(e) => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                format!("工作区不存在: {e}"),
            ));
        }
    }

    let ws = workspace.clone();
    let q_task = q.clone();
    let (ranked, truncated) = tokio::task::spawn_blocking(move || {
        let (candidates, capped) = super::find::walk_workspace(&ws, q_task.is_empty());
        let mut scored: Vec<(i64, Option<std::time::SystemTime>, String, Vec<usize>)> =
            if q_task.is_empty() {
                candidates
                    .into_iter()
                    .map(|c| (0, c.mtime, c.path, Vec::new()))
                    .collect()
            } else {
                candidates
                    .into_iter()
                    .filter_map(|c| {
                        super::find::fuzzy_match(&q_task, &c.path)
                            .map(|m| (m.score, c.mtime, c.path, m.indices))
                    })
                    .collect()
            };
        if q_task.is_empty() {
            scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
        } else {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
        }
        let over = scored.len() > limit;
        scored.truncate(limit);
        (scored, capped || over)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("文件查找失败: {e}"),
        )
    })?;

    let entries = ranked
        .into_iter()
        .map(|(_, _, path, indices)| {
            let size = std::fs::metadata(workspace.join(&path)).map_or(0, |m| m.len());
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            FsFindEntry {
                entry: FsEntry {
                    name,
                    path,
                    is_dir: false,
                    size,
                },
                indices,
            }
        })
        .collect();

    Ok(Json(FsFindResponse {
        workspace: workspace.to_string_lossy().to_string(),
        query: q,
        entries,
        truncated,
    }))
}

/// 列出工程目录下的条目（目录与文件），支持工程文件浏览器懒加载
pub async fn list_fs_entries(
    State(state): State<AppState>,
    Query(query): Query<FsEntriesQuery>,
) -> ApiResult<Json<FsEntriesResponse>> {
    let workspace = workspace_or_default(&state, query.workspace.as_deref());
    let subpath = query.path.as_deref().unwrap_or("").trim();
    let target_dir = crate::tools::filesystem::resolve_path(&workspace, subpath);

    match tokio::fs::metadata(&target_dir).await {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "目标路径不是目录"));
        }
        Err(e) => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                format!("目录不存在: {e}"),
            ));
        }
    }

    let mut read_dir = tokio::fs::read_dir(&target_dir).await.map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("读取目录失败: {e}"),
        )
    })?;

    let slash_rel = |path: &StdPath| {
        path.strip_prefix(&workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut entries = Vec::new();
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        // 过滤内部版本控制系统目录
        if name == ".git" {
            continue;
        }
        let entry_path = entry.path();
        let Ok(meta) = tokio::fs::metadata(&entry_path).await else {
            continue;
        };
        entries.push(FsEntry {
            name,
            path: slash_rel(&entry_path),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
        });
    }

    // 排序：目录优先，同类按名称不区分大小写排序
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    let path = match target_dir.strip_prefix(&workspace) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => String::new(),
    };
    Ok(Json(FsEntriesResponse {
        workspace: workspace.to_string_lossy().to_string(),
        path,
        entries,
    }))
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
) -> ApiResult<Response> {
    let model = request
        .model
        .unwrap_or_else(|| state.manager.app_config().settings.default.clone());
    let search = request.search.unwrap_or(true);
    let handle = match request.workspace.as_deref().map(str::trim) {
        Some(workspace) if !workspace.is_empty() => {
            state
                .manager
                .create_session_in(workspace, model, request.variant, None, search)
                .await
        }
        _ => {
            state
                .manager
                .create_session(model, request.variant, None, search)
                .await
        }
    }
    .map_err(fail(StatusCode::BAD_REQUEST))?;
    Ok(Session(handle).snapshot_json())
}

pub async fn get_session(session: Session) -> Response {
    session.snapshot_json()
}

pub async fn delete_session(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<StatusCode> {
    state
        .manager
        .delete_session(&session.0)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(StatusCode::NO_CONTENT)
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
) -> ApiResult<Json<SessionFileResponse>> {
    let workspace = workspace_or_default(&state, query.workspace.as_deref());
    read_file_from_workspace(&workspace, &query.path).await
}

/// 读取会话相关文件的实时内容，供前端侧栏预览
pub async fn get_session_file(
    session: Session,
    Query(query): Query<SessionFileQuery>,
) -> ApiResult<Json<SessionFileResponse>> {
    read_file_from_workspace(&session.workspace(), &query.path).await
}

/// 下载指定工作区文件，保留原始二进制内容
pub async fn download_fs_file(
    State(state): State<AppState>,
    Query(query): Query<FsFileQuery>,
) -> ApiResult<Response> {
    let workspace = workspace_or_default(&state, query.workspace.as_deref());
    download_file_from_workspace(&workspace, &query.path).await
}

/// 下载会话工作区文件，保留原始二进制内容
pub async fn download_session_file(
    session: Session,
    Query(query): Query<SessionFileQuery>,
) -> ApiResult<Response> {
    download_file_from_workspace(&session.workspace(), &query.path).await
}

async fn read_bytes(path: &StdPath) -> ApiResult<Vec<u8>> {
    tokio::fs::read(path).await.map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("读取文件失败: {e}"),
        )
    })
}

async fn download_file_from_workspace(workspace: &StdPath, rel_path: &str) -> ApiResult<Response> {
    let (path, _) = open_workspace_file(workspace, rel_path).await?;
    let bytes = read_bytes(&path).await?;
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
    Ok(response)
}

/// 解析工作区内相对路径并校验为存在的文件；缺失或目录转为错误响应
async fn open_workspace_file(
    workspace: &StdPath,
    rel_path: &str,
) -> ApiResult<(PathBuf, std::fs::Metadata)> {
    let path = crate::tools::filesystem::resolve_path(workspace, rel_path);
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| ApiError::new(StatusCode::NOT_FOUND, format!("文件不存在: {e}")))?;
    if metadata.is_dir() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "目标是目录而非文件"));
    }
    Ok((path, metadata))
}

/// 从指定工作区读取文件内容并返回统一的 SessionFileResponse 响应
pub async fn read_file_from_workspace(
    workspace: &StdPath,
    rel_path: &str,
) -> ApiResult<Json<SessionFileResponse>> {
    let (path, metadata) = open_workspace_file(workspace, rel_path).await?;
    let size = metadata.len();
    if size > 10 * 1024 * 1024 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "文件过大，不支持在线预览 (超过 10MB)",
        ));
    }
    let bytes = read_bytes(&path).await?;

    let mime_type = mime_guess::from_path(&path).first().map(|m| m.to_string());
    let text = std::str::from_utf8(&bytes)
        .ok()
        .filter(|text| !text.contains('\0'));
    let is_image = mime_type
        .as_deref()
        .is_some_and(|m| m.starts_with("image/"));
    let (is_binary, content) = match text {
        Some(text) => (false, text.to_string()),
        None if is_image => (
            true,
            base64::engine::general_purpose::STANDARD.encode(&bytes),
        ),
        None => (true, String::new()),
    };
    Ok(Json(SessionFileResponse {
        path: path.to_string_lossy().into_owned(),
        name: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into()),
        size,
        is_binary,
        content,
        mime_type,
    }))
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

impl From<SubmitFile> for ContentPart {
    fn from(file: SubmitFile) -> Self {
        match file.mime_type.as_str() {
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" => ContentPart::Image {
                source: MediaSource::Base64 {
                    data: file.data,
                    mime_type: file.mime_type,
                },
                mime_type: None,
            },
            _ => ContentPart::File {
                source: MediaSource::Base64 {
                    data: file.data,
                    mime_type: file.mime_type.clone(),
                },
                mime_type: file.mime_type,
                filename: file.filename,
            },
        }
    }
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
) -> ApiResult<Json<serde_json::Value>> {
    let filename = StdPath::new(&request.filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("upload");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&request.data)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, format!("Invalid file data: {e}")))?;
    let workspace = workspace_or_default(&state, request.workspace.as_deref());
    let scratch = state
        .manager
        .store()
        .ensure_scratch(&workspace)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    let path = scratch.join(format!("{}_{}", uuid::Uuid::new_v4(), filename));
    tokio::fs::write(&path, bytes)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::json!({ "path": path })))
}

pub async fn submit_message(
    session: Session,
    Json(request): Json<SubmitMessageRequest>,
) -> ApiResult<StatusCode> {
    let files = request.files.into_iter().map(ContentPart::from).collect();
    // 会话正在执行其他请求时拒绝重复提交
    session
        .0
        .submit(&request.prompt, files, None)
        .await
        .map_err(fail(StatusCode::CONFLICT))?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub struct RevertRequest {
    pub turn_index: usize,
}

/// 回退到指定用户轮次：丢弃该轮次及其后的全部内容
pub async fn revert_session(
    session: Session,
    Json(request): Json<RevertRequest>,
) -> ApiResult<Response> {
    session
        .0
        .revert(request.turn_index)
        .await
        .map_err(fail(StatusCode::BAD_REQUEST))?;
    Ok(session.snapshot_json())
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
    session: Session,
    Json(request): Json<ForkRequest>,
) -> ApiResult<Response> {
    let created = state
        .manager
        .fork_session(
            &session.0,
            request.turn_index,
            request.title.unwrap_or_default(),
        )
        .await
        .map_err(fail(StatusCode::BAD_REQUEST))?;
    Ok(Session(created).snapshot_json())
}

pub async fn abort_session(session: Session) -> ApiResult<StatusCode> {
    session
        .0
        .abort_current()
        .await
        .map_err(fail(StatusCode::CONFLICT))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct DismissNoticeRequest {
    pub id: String,
}

/// 关闭一条运行通知横幅：按稳定 ID 移除并发布
pub async fn dismiss_notice(
    session: Session,
    Json(request): Json<DismissNoticeRequest>,
) -> ApiResult<StatusCode> {
    session
        .0
        .dismiss_notice(request.id)
        .await
        .map_err(fail(StatusCode::CONFLICT))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AnswerUserRequest {
    pub request_id: String,
    pub answers: Vec<UserAnswer>,
}

pub async fn answer_user(
    session: Session,
    Json(request): Json<AnswerUserRequest>,
) -> ApiResult<StatusCode> {
    session
        .0
        .answer_user(request.request_id, request.answers)
        .await
        .map_err(fail(StatusCode::BAD_REQUEST))?;
    Ok(StatusCode::ACCEPTED)
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
    session: Session,
    Json(request): Json<ConfigureSessionRequest>,
) -> ApiResult<Response> {
    session
        .0
        .configure(request.model, request.variant, request.search)
        .await
        .map_err(fail(StatusCode::BAD_REQUEST))?;
    Ok(session.snapshot_json())
}

/// 会话统计 (Token 消耗、缓存命中与预估花费)，复用 TUI 侧栏的 compute_stats 投影
pub async fn session_stats(State(state): State<AppState>, session: Session) -> Json<SidebarStats> {
    Json(compute_stats(
        &session.0.snapshot().session,
        &state.manager.app_config(),
    ))
}

/// 全局配置文件寻址：`~/.vva/config.jsonc` 优先，其次 `config.json`，
/// 两者都不存在时以 `config.json` 作为写入目标
fn global_config_path() -> ApiResult<PathBuf> {
    let dir = crate::config::home_dir()
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法定位用户主目录 (~)"))?
        .join(".vva");
    let jsonc = dir.join("config.jsonc");
    Ok(if jsonc.exists() {
        jsonc
    } else {
        dir.join("config.json")
    })
}

/// 读取全局配置文件原文（JSONC），供 Settings 弹窗编辑；文件不存在时内容为 null
pub async fn get_global_config() -> ApiResult<Json<serde_json::Value>> {
    let path = global_config_path()?;
    let content = tokio::fs::read_to_string(&path).await.ok();
    Ok(Json(serde_json::json!({
        "path": path.display().to_string(),
        "content": content,
    })))
}

#[derive(Deserialize)]
pub struct UpdateConfigRequest {
    pub content: String,
}

/// 保存全局配置：先按完整解析流程校验，落盘后热重载合并配置供后续会话使用
pub async fn update_global_config(
    State(state): State<AppState>,
    Json(request): Json<UpdateConfigRequest>,
) -> ApiResult<StatusCode> {
    let path = global_config_path()?;
    crate::config::parse_config_str(&request.content).map_err(fail(StatusCode::BAD_REQUEST))?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    }
    tokio::fs::write(&path, &request.content)
        .await
        .map_err(fail(StatusCode::INTERNAL_SERVER_ERROR))?;
    state.manager.reload_config().map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("配置已保存，但热重载失败: {e}"),
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// 每条连接以快照建立基线，再发送相对已发送版本的增量。
/// watch 可合并中间发布；慢客户端仍从自己的基线更新到最新状态。
pub async fn stream_events(session: Session) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = session.0.subscribe();
    let initial = rx.borrow_and_update().clone();
    let stream = futures::stream::unfold(
        (rx, initial, true),
        |(mut rx, previous, first)| async move {
            if first {
                let event = Event::default()
                    .event("snapshot")
                    .json_data(SessionSnapshot::from(&*previous))
                    .expect("session snapshot is serializable");
                return Some((Ok(event), (rx, previous, false)));
            }
            rx.changed().await.ok()?;
            let current = rx.borrow_and_update().clone();
            let event = Event::default()
                .event("delta")
                .json_data(SessionDelta::between(&previous, &current))
                .expect("session delta is serializable");
            Some((Ok(event), (rx, current, false)))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::SessionManager;
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

        let list = |path: Option<&str>| {
            list_fs_entries(
                State(state.clone()),
                Query(FsEntriesQuery {
                    workspace: Some(ws.to_string_lossy().to_string()),
                    path: path.map(String::from),
                }),
            )
        };

        let Json(res) = list(None).await.ok().unwrap();
        assert!(!res.entries.iter().any(|e| e.name == ".git"));
        assert_eq!(res.entries[0].name, "src");
        assert!(res.entries[0].is_dir);
        assert_eq!(res.entries[0].path, "src");

        let file_names: Vec<&str> = res.entries[1..].iter().map(|e| e.name.as_str()).collect();
        assert_eq!(file_names, vec!["Cargo.toml", "README.md"]);

        let Json(sub_res) = list(Some("src")).await.ok().unwrap();
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

        let Json(res) = read_file_from_workspace(ws, "hello.txt")
            .await
            .ok()
            .unwrap();
        assert_eq!(res.name, "hello.txt");
        assert_eq!(res.content, "hello world");
        assert!(!res.is_binary);

        let missing = read_file_from_workspace(ws, "nonexistent.txt").await.err();
        assert_eq!(missing.unwrap().0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn overlay_session_states_attaches_live_run_status() {
        let idle = SessionSummary {
            id: "sess_idle".into(),
            title: Some("Idle".into()),
            workspace_root: "/tmp/ws".into(),
            created_at: chrono::Utc::now(),
            turn_count: 1,
            current_model: "openai/gpt".into(),
            current_variant: None,
            web_search: true,
            parent: None,
            agent: None,
        };
        let running = SessionSummary {
            id: "sess_run".into(),
            title: Some("Run".into()),
            ..idle.clone()
        };
        let live = HashMap::from([("sess_run".into(), "Thinking...".into())]);
        let items = overlay_session_states(vec![idle, running], &live);
        assert!(items[0].state.is_empty());
        assert_eq!(items[1].state, "Thinking...");
        let json = serde_json::to_value(&items).unwrap();
        assert!(json[0].get("state").is_none());
        assert_eq!(json[1]["state"], "Thinking...");
    }
}
