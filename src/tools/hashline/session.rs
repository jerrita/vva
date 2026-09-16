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

#[derive(Clone)]
pub(super) struct FileSnap {
    pub(super) text: String,
    pub(super) seen: HashSet<u32>,
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
        let tag = file_tag(text);
        let mut inner = self.lock()?;
        let snaps = inner.files.entry(canonical).or_default();
        let snap = snaps.entry(tag.clone()).or_insert_with(|| FileSnap {
            text: text.to_string(),
            seen: HashSet::new(),
        });
        if snap.text != text {
            snap.text = text.to_string();
            snap.seen.clear();
        }
        snap.seen.extend(seen);
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
                    "not_found: snapshot [{}#{}] is not registered in this active session (including after session reload); this does not indicate a disk change. Read or Grep the file and copy the header",
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
    format!("{:x}", Sha256::digest(text.as_bytes()))[..4].to_ascii_uppercase()
}
