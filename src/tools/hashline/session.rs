//! 会话级快照登记：`[PATH#TAG]` 文件指纹与已展示行号集合。

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct HashlineSession {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    files: HashMap<PathBuf, HashMap<String, FileSnap>>,
    named: HashMap<String, Vec<String>>,
}

#[derive(Clone, Default)]
pub(super) struct FileSnap {
    pub(super) text: String,
    pub(super) seen: HashSet<u32>,
    /// 部分展示的长行：行号 -> 已展示字节数（byte_offset 续读进度）。
    pub(super) partial: HashMap<u32, usize>,
}

impl HashlineSession {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, String> {
        self.inner
            .lock()
            .map_err(|_| "hashline snapshot lock poisoned".into())
    }

    pub fn record(
        &self,
        canonical: PathBuf,
        text: &str,
        seen: impl IntoIterator<Item = u32>,
    ) -> Result<String, String> {
        self.record_with_partial(canonical, text, seen, Vec::new())
    }

    /// 同 [`record`](Self::record)，另登记 byte_offset 部分展示进度；
    /// 行完整展示（计入 seen）时自动清除对应进度。
    pub(super) fn record_with_partial(
        &self,
        canonical: PathBuf,
        text: &str,
        seen: impl IntoIterator<Item = u32>,
        partial: Vec<(u32, usize)>,
    ) -> Result<String, String> {
        let tag = file_tag(text);
        let mut inner = self.lock()?;
        let snaps = inner.files.entry(canonical).or_default();
        let snap = snaps.entry(tag.clone()).or_insert_with(Default::default);
        if snap.text != text {
            snap.text = text.to_string();
            snap.seen.clear();
            snap.partial.clear();
        }
        let shown: HashSet<u32> = seen.into_iter().collect();
        snap.seen.extend(shown.iter().copied());
        snap.partial.retain(|line, _| !snap.seen.contains(line));
        for (line, bytes) in partial {
            snap.partial.insert(line, bytes);
        }
        Ok(tag)
    }

    pub(super) fn lookup(&self, canonical: &Path, tag: &str) -> Result<FileSnap, String> {
        let inner = self.lock()?;
        inner
            .files
            .get(canonical)
            .and_then(|snaps| snaps.get(tag))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "not_found: snapshot [{}#{}] is not registered in this active session (including after session reload); this does not indicate a disk change. hashline_read or hashline_grep the file and copy the header",
                    canonical.display(),
                    tag
                )
            })
    }

    pub(super) fn publish_named(&self, named: HashMap<String, Vec<String>>) -> Result<(), String> {
        if named.is_empty() {
            return Ok(());
        }
        self.lock()?.named.extend(named);
        Ok(())
    }

    pub(super) fn named(&self, name: &str) -> Result<Option<Vec<String>>, String> {
        Ok(self.lock()?.named.get(name).cloned())
    }
}

pub fn file_tag(text: &str) -> String {
    Sha256::digest(text.as_bytes())[..2]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect()
}
