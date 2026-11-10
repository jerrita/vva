use super::records::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};
use vva_core::{
    model::{EditorKind, SessionHeader as CoreHeader, Turn},
    storage::{SessionStore as _, jsonl::JsonlSessionStore, project_key},
};
fn default_web_search() -> bool {
    true
}
fn is_zero_depth(depth: &u8) -> bool {
    *depth == 0
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct LegacyHeader {
    version: u32,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    workspace_root: PathBuf,
    created_at: DateTime<Utc>,
    #[serde(default)]
    current_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_variant: Option<String>,
    #[serde(default = "default_web_search")]
    web_search: bool,
    editor: EditorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_depth")]
    depth: u8,
    /// agent space 会话类别；缺省 Topic 不序列化
    #[serde(default, skip_serializing_if = "SessionKind::is_topic")]
    kind: SessionKind,
    /// 会话来源 IM 位置（agent space 创建的会话携带）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<Origin>,
    /// 路由本会话的决策来源（如 `reply` / `switch` / `router`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routed_by: Option<String>,
    /// rollover 接替会话所承接的旧会话 id（与 subagent 的 `parent` 语义分离）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continued_from: Option<String>,
    /// rollover 后接替本会话的新会话 id；router 排除已被取代的会话
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    /// 最近一次用户活动时刻，TTL 清理依据（重启后据此恢复）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_activity: Option<DateTime<Utc>>,
    /// 会话空闲 TTL（秒）；未指定时使用 agent space 配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ttl_secs: Option<u64>,
    /// 当前会话请求在清理前沉淀；缺省不整理
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    curation_requested: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LegacyRecord {
    Header(Box<LegacyHeader>),
    Turn(Box<Turn>),
}
#[derive(Debug, Serialize)]
pub struct MigrationEntry {
    pub session_id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub application_record: PathBuf,
    pub turns: usize,
    pub resources: ResourcePaths,
}
#[derive(Debug, Serialize)]
pub struct MigrationManifest {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub application_directory: PathBuf,
    pub sessions: Vec<MigrationEntry>,
}

/// 新二进制的写入与迁移共用目录旁的锁；旧二进制仍须调用者显式停写。
pub fn writer_lock(directory: &Path, exclusive: bool) -> Result<File, String> {
    let directory = std::path::absolute(directory).map_err(|e| e.to_string())?;
    let name = directory
        .file_name()
        .ok_or("Session directory must have a name")?
        .to_string_lossy();
    let parent = directory
        .parent()
        .ok_or("Session directory must have a parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join(format!(".{name}.writer.lock")))
        .map_err(|e| e.to_string())?;
    let locked = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    locked.map_err(|e| {
        format!("Session directory is in use; stop all writers before migration: {e}")
    })?;
    Ok(file)
}

pub async fn migrate(
    source: &Path,
    destination: &Path,
    stopped: bool,
) -> Result<MigrationManifest, String> {
    if !stopped {
        return Err("Stop all vva writers, then pass --stopped to acknowledge the offline migration precondition".into());
    }
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let destination = std::path::absolute(destination).map_err(|e| e.to_string())?;
    let parent = destination
        .parent()
        .ok_or("Destination must have a parent")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let name = destination
        .file_name()
        .ok_or("Destination must have a name")?;
    let destination = parent.join(name);
    let application_directory =
        destination.with_file_name(format!("{}-app", name.to_string_lossy()));
    if destination.starts_with(&source) || application_directory.starts_with(&source) {
        return Err("Migration destination must be outside the source directory".into());
    }
    if destination.exists() || application_directory.exists() {
        return Err("Migration destination and application-record directory must not exist".into());
    }
    let _source_lock = writer_lock(&source, true)?;
    let _destination_lock = writer_lock(&destination, true)?;
    let mut files = Vec::new();
    for entry in fs::read_dir(&source).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                {
                    files.push(path);
                }
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    files.sort();
    let mut ids = BTreeSet::new();
    let mut loaded = Vec::new();
    let mut entries = Vec::new();
    for path in files {
        let mut lines = BufReader::new(File::open(&path).map_err(|e| e.to_string())?).lines();
        let first = lines
            .next()
            .ok_or_else(|| format!("Empty session: {}", path.display()))?
            .map_err(|e| e.to_string())?;
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let version: Version =
            serde_json::from_str(&first).map_err(|e| format!("{}: {e}", path.display()))?;
        if version.version != 2 {
            return Err(format!(
                "{}: expected v2, found version {}",
                path.display(),
                version.version
            ));
        }
        let LegacyRecord::Header(header) =
            serde_json::from_str(&first).map_err(|e| format!("{}: {e}", path.display()))?
        else {
            return Err("First line must be Header".into());
        };
        if path.file_stem().and_then(|s| s.to_str()) != Some(header.id.as_str()) {
            return Err(format!(
                "Session ID does not match file: {}",
                path.display()
            ));
        }
        if !ids.insert(header.id.clone()) {
            return Err(format!("Duplicate SessionId: {}", header.id));
        }
        let mut turns = Vec::new();
        for line in lines {
            match serde_json::from_str::<LegacyRecord>(&line.map_err(|e| e.to_string())?)
                .map_err(|e| format!("{}: {e}", path.display()))?
            {
                LegacyRecord::Turn(turn) => turns.push(*turn),
                LegacyRecord::Header(_) => return Err("Unexpected additional Header".into()),
            }
        }
        let project = if path.parent() == Some(source.as_path()) {
            source.join(project_key(&header.workspace_root))
        } else {
            path.parent()
                .expect("session file has parent")
                .to_path_buf()
        };
        let resources = ResourcePaths {
            scratch: project.join("scratch"),
            tool_outputs: project.join("tool_outputs"),
        };
        let application = if header.origin.is_some() || header.kind == SessionKind::Curator {
            Application::Space(SpaceSessionRecord {
                origin: header.origin,
                kind: header.kind,
                routed_by: header.routed_by,
                ttl_secs: header.ttl_secs,
                last_activity: header.last_activity,
                curation_requested: header.curation_requested,
            })
        } else {
            Application::Coding
        };
        let record = SessionRecord {
            id: header.id.clone(),
            coding: CodingSessionConfig {
                editor: header.editor,
                web_search: header.web_search,
            },
            application,
            resources: resources.clone(),
        };
        let core = CoreHeader {
            version: header.version + 1,
            id: header.id.clone(),
            title: header.title,
            workspace_root: header.workspace_root,
            created_at: header.created_at,
            current_model: header.current_model,
            current_variant: header.current_variant,
            parent: header.parent,
            agent: header.agent,
            depth: header.depth,
            continued_from: header.continued_from,
            superseded_by: header.superseded_by,
        };
        entries.push(MigrationEntry {
            session_id: header.id.clone(),
            source: path,
            destination: destination
                .join(project_key(&core.workspace_root))
                .join(format!("{}.jsonl", header.id)),
            application_record: application_directory.join(format!("{}.json", header.id)),
            turns: turns.len(),
            resources,
        });
        loaded.push((core.into_session(turns), record));
    }
    let staging = tempfile::tempdir_in(&parent).map_err(|e| e.to_string())?;
    let staged_sessions = staging.path().join("sessions");
    let staged_records = staging.path().join("records");
    fs::create_dir(&staged_records).map_err(|e| e.to_string())?;
    let store = JsonlSessionStore::new(&staged_sessions).map_err(|e| e.to_string())?;
    for (session, record) in loaded {
        store
            .save_header(&session)
            .await
            .map_err(|e| e.to_string())?;
        let mut file = File::create(staged_records.join(format!("{}.json", record.id)))
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, &record).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    let manifest = MigrationManifest {
        source,
        destination: destination.clone(),
        application_directory: application_directory.clone(),
        sessions: entries,
    };
    let mut file =
        File::create(staged_sessions.join("migration-manifest.json")).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, &manifest).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(&staged_records, &application_directory).map_err(|e| e.to_string())?;
    fs::rename(&staged_sessions, &destination).map_err(|e| {
        format!(
            "Application records are at {}; session-directory activation failed: {e}",
            application_directory.display()
        )
    })?;
    Ok(manifest)
}
