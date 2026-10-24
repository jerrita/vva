//! binding（`channel:chat_id`）到 focus session 的绑定、message_id 反查索引与待附件缓冲。

use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::model::Origin;

const BINDINGS_FILE: &str = "bindings.json";
const MESSAGE_INDEX_FILE: &str = "message_index.jsonl";

/// 单个 binding 的持久状态
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BindingState {
    /// 默认续写的 session；无明确锚点时仅作为 router 的参考
    focus: Option<String>,
    /// 当前聊天是否接收 agent space 后台通知。
    #[serde(default)]
    home: bool,
}

/// `<agent_dir>/bindings.json`：每次变更原子落盘（临时文件 + rename）
#[derive(Debug)]
pub struct BindingStore {
    path: PathBuf,
    bindings: BTreeMap<String, BindingState>,
}

impl BindingStore {
    /// 文件不存在视为全新空间；存在但无法解析则显式报错
    pub fn load(agent_dir: &Path) -> Result<Self, String> {
        let path = agent_dir.join(BINDINGS_FILE);
        let bindings = match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| format!("解析 {} 失败: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
        };
        Ok(Self { path, bindings })
    }

    pub fn focus(&self, binding: &str) -> Option<&str> {
        self.bindings.get(binding)?.focus.as_deref()
    }

    pub fn is_home(&self, binding: &str) -> bool {
        self.bindings.get(binding).is_some_and(|state| state.home)
    }

    pub fn homes(&self) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|(_, state)| state.home)
            .map(|(binding, _)| binding.clone())
            .collect()
    }

    pub fn set_home(&mut self, binding: &str, home: bool) -> Result<(), String> {
        if self.is_home(binding) == home {
            return Ok(());
        }
        let previous = self.bindings.get(binding).cloned();
        self.bindings.entry(binding.to_string()).or_default().home = home;
        if let Err(error) = self.save() {
            match previous {
                Some(state) => {
                    self.bindings.insert(binding.to_string(), state);
                }
                None => {
                    self.bindings.remove(binding);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn set_focus(&mut self, binding: &str, session_id: Option<String>) -> Result<(), String> {
        tracing::debug!("focus 变更: binding={binding} -> {session_id:?}");
        self.bindings.entry(binding.to_string()).or_default().focus = session_id;
        self.save()
    }

    /// session 被删除或被取代时，清除或改指所有以它为 focus 的 binding
    pub fn replace_focus(&mut self, old: &str, new: Option<&str>) -> Result<(), String> {
        for state in self.bindings.values_mut() {
            if state.focus.as_deref() == Some(old) {
                tracing::debug!("focus 改指: {old} -> {new:?}");
                state.focus = new.map(str::to_string);
            }
        }
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.bindings).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("写入 {} 失败: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("替换 {} 失败: {e}", self.path.display()))
    }
}

/// QQ 群聊 binding；渠道名独立于聊天类型，便于渠道桩复用。
pub(super) fn is_group_binding(binding: &str) -> bool {
    binding
        .split_once(':')
        .is_some_and(|(_, chat_id)| chat_id.starts_with("group:"))
}

/// 群聊只访问本群会话；个人聊天共享排除群聊来源的个人会话池。
pub(super) fn in_chat_scope(binding: &str, origin: Option<&Origin>) -> bool {
    if is_group_binding(binding) {
        origin.is_some_and(|origin| origin.binding() == binding)
    } else {
        !origin.is_some_and(|origin| is_group_binding(&origin.binding()))
    }
}

#[derive(Serialize, Deserialize)]
struct IndexRecord {
    message_id: String,
    session_id: String,
}

/// `<agent_dir>/message_index.jsonl`：消息 id → session id，追加写；同 id 后写覆盖先写。
/// 调用方应传入已含 channel/binding 前缀的全局唯一 message_id
#[derive(Debug)]
pub struct MessageIndex {
    path: PathBuf,
    map: HashMap<String, String>,
}

impl MessageIndex {
    pub fn load(agent_dir: &Path) -> Result<Self, String> {
        let path = agent_dir.join(MESSAGE_INDEX_FILE);
        let mut map = HashMap::new();
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                for (i, line) in raw.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
                    let rec: IndexRecord = serde_json::from_str(line)
                        .map_err(|e| format!("{}:{} 解析失败: {e}", path.display(), i + 1))?;
                    map.insert(rec.message_id, rec.session_id);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
        }
        Ok(Self { path, map })
    }

    pub fn get(&self, message_id: &str) -> Option<&str> {
        self.map.get(message_id).map(String::as_str)
    }

    pub fn record(&mut self, message_id: &str, session_id: &str) -> Result<(), String> {
        self.commit(vec![IndexRecord {
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
        }])
    }

    /// rollover：把指向旧 session 的全部消息改指新 session（追加覆盖行）
    pub fn repoint(&mut self, old: &str, new: &str) -> Result<(), String> {
        let moved = self
            .map
            .iter()
            .filter(|(_, s)| s.as_str() == old)
            .map(|(m, _)| IndexRecord {
                message_id: m.clone(),
                session_id: new.to_string(),
            })
            .collect();
        self.commit(moved)
    }

    /// 先追加落盘，成功后再更新内存映射
    fn commit(&mut self, records: Vec<IndexRecord>) -> Result<(), String> {
        if records.is_empty() {
            return Ok(());
        }
        let mut buf = String::new();
        for rec in &records {
            buf.push_str(&serde_json::to_string(rec).expect("IndexRecord 仅含字符串字段"));
            buf.push('\n');
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(buf.as_bytes()))
            .map_err(|e| format!("追加 {} 失败: {e}", self.path.display()))?;
        self.map
            .extend(records.into_iter().map(|r| (r.message_id, r.session_id)));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Image,
    File,
}

/// 已落盘的入站附件；序列化给 router 时只暴露文件名与类型
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttachedFile {
    #[serde(skip)]
    pub path: PathBuf,
    pub name: String,
    pub kind: FileKind,
}

#[derive(Debug)]
struct PendingFile {
    file: AttachedFile,
    received_at: DateTime<Utc>,
}

/// 待附件缓冲（内存态）：收到无文字文件时暂存，下一条带文字消息一并路由
#[derive(Debug, Default)]
pub struct PendingFiles {
    by_binding: HashMap<String, Vec<PendingFile>>,
}

impl PendingFiles {
    pub fn push(&mut self, binding: &str, file: AttachedFile, now: DateTime<Utc>) {
        self.by_binding
            .entry(binding.to_string())
            .or_default()
            .push(PendingFile {
                file,
                received_at: now,
            });
    }

    pub fn count(&self, binding: &str) -> usize {
        self.by_binding.get(binding).map_or(0, Vec::len)
    }

    /// 取出并清空该 binding 的缓冲，保持接收顺序
    pub fn take(&mut self, binding: &str) -> Vec<AttachedFile> {
        self.by_binding
            .remove(binding)
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.file)
            .collect()
    }

    /// 移除等待超过 `ttl`（自最早一个文件起算）的 binding，返回其文件供上层提示用户
    pub fn expire(
        &mut self,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Vec<(String, Vec<AttachedFile>)> {
        let expired: Vec<String> = self
            .by_binding
            .iter()
            .filter(|(_, files)| files.first().is_some_and(|f| now - f.received_at >= ttl))
            .map(|(b, _)| b.clone())
            .collect();
        expired
            .into_iter()
            .map(|b| {
                let files = self.take(&b);
                (b, files)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> AttachedFile {
        AttachedFile {
            path: PathBuf::from("/tmp").join(name),
            name: name.into(),
            kind: FileKind::File,
        }
    }

    #[test]
    fn bindings_roundtrip_and_replace() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BindingStore::load(dir.path()).unwrap();
        assert_eq!(store.focus("qq:c2c:1"), None);
        store.set_focus("qq:c2c:1", Some("sess_a".into())).unwrap();
        store.set_focus("qq:c2c:2", Some("sess_a".into())).unwrap();
        store.set_focus("qq:c2c:3", Some("sess_b".into())).unwrap();

        let mut reloaded = BindingStore::load(dir.path()).unwrap();
        assert_eq!(reloaded.focus("qq:c2c:1"), Some("sess_a"));
        reloaded.replace_focus("sess_a", Some("sess_c")).unwrap();
        reloaded.replace_focus("sess_b", None).unwrap();

        let again = BindingStore::load(dir.path()).unwrap();
        assert_eq!(again.focus("qq:c2c:1"), Some("sess_c"));
        assert_eq!(again.focus("qq:c2c:2"), Some("sess_c"));
        assert_eq!(again.focus("qq:c2c:3"), None);
        assert!(!dir.path().join("bindings.json.tmp").exists());
    }

    #[test]
    fn homes_upgrade_legacy_state_and_survive_focus_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(BINDINGS_FILE),
            r#"{"qq:c2c:owner":{"focus":"sess_old"}}"#,
        )
        .unwrap();
        let mut store = BindingStore::load(dir.path()).unwrap();
        assert!(store.homes().is_empty());
        store.set_home("qq:c2c:owner", true).unwrap();
        store.set_home("qq:c2c:owner", true).unwrap();
        store.set_home("qq:group:service", true).unwrap();
        store.replace_focus("sess_old", Some("sess_new")).unwrap();
        let mut store = BindingStore::load(dir.path()).unwrap();
        assert_eq!(store.homes(), ["qq:c2c:owner", "qq:group:service"]);
        assert_eq!(store.focus("qq:c2c:owner"), Some("sess_new"));
        store.set_home("qq:c2c:owner", false).unwrap();
        store.set_home("qq:c2c:owner", false).unwrap();
        let store = BindingStore::load(dir.path()).unwrap();
        assert_eq!(store.homes(), ["qq:group:service"]);
        assert_eq!(store.focus("qq:c2c:owner"), Some("sess_new"));
    }

    #[test]
    fn message_index_roundtrip_and_repoint() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = MessageIndex::load(dir.path()).unwrap();
        idx.record("m1", "sess_a").unwrap();
        idx.record("m2", "sess_a").unwrap();
        idx.record("m3", "sess_b").unwrap();
        idx.repoint("sess_a", "sess_c").unwrap();
        assert_eq!(idx.get("m1"), Some("sess_c"));

        let reloaded = MessageIndex::load(dir.path()).unwrap();
        assert_eq!(reloaded.get("m1"), Some("sess_c"));
        assert_eq!(reloaded.get("m2"), Some("sess_c"));
        assert_eq!(reloaded.get("m3"), Some("sess_b"));
        assert_eq!(reloaded.get("nope"), None);
    }

    #[test]
    fn pending_files_expire_by_oldest() {
        let t0 = Utc::now();
        let mut pending = PendingFiles::default();
        pending.push("old", file("a"), t0);
        pending.push("old", file("b"), t0 + Duration::seconds(500));
        pending.push("fresh", file("c"), t0 + Duration::seconds(550));
        let ttl = Duration::seconds(600);

        assert!(pending.expire(t0 + Duration::seconds(599), ttl).is_empty());
        let expired = pending.expire(t0 + Duration::seconds(600), ttl);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "old");
        let names: Vec<_> = expired[0].1.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(pending.count("old"), 0);
        assert_eq!(pending.take("fresh").len(), 1);
    }
}
