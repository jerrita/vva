use super::{
    model::{Session, SessionSummary},
    records::*,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use vva_core::storage::{SessionStore as CoreStore, StoreError, jsonl::JsonlSessionStore};

pub struct AppSessionStore {
    pub core: Arc<dyn CoreStore>,
    pub base: PathBuf,
    pub records: PathBuf,
    mutation: Mutex<()>,
    slots: Mutex<BTreeMap<String, Arc<watch::Sender<SessionRecord>>>>,
}
impl AppSessionStore {
    pub fn new(base: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let base = std::path::absolute(base.into())?;
        let name = base
            .file_name()
            .ok_or_else(|| StoreError::InvalidFormat("Session directory must have a name".into()))?
            .to_string_lossy();
        let records = base.with_file_name(format!("{name}-app"));
        Ok(Self {
            core: Arc::new(JsonlSessionStore::new(&base)?),
            base,
            records,
            mutation: Mutex::new(()),
            slots: Mutex::new(BTreeMap::new()),
        })
    }
    #[cfg(test)]
    pub(crate) fn with_core(core: Arc<dyn CoreStore>, base: PathBuf) -> Self {
        Self {
            core,
            records: base.with_file_name(format!(
                "{}-app",
                base.file_name()
                    .expect("test fixture directory has a name")
                    .to_string_lossy()
            )),
            base,
            mutation: Mutex::new(()),
            slots: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn slot(&self, id: &str) -> Result<Arc<watch::Sender<SessionRecord>>, StoreError> {
        let path = self.records.join(format!("{id}.json"));
        let mut slots = self.slots.lock().expect("application records lock");
        let file = std::fs::File::open(&path).map_err(|error| {
            StoreError::InvalidFormat(format!(
                "Application record {} unavailable: {error}",
                path.display()
            ))
        })?;
        let record: SessionRecord = serde_json::from_reader(file)?;
        if record.id != id {
            return Err(StoreError::InvalidFormat(format!(
                "Application record ID mismatch: {id}"
            )));
        }
        if let Some(slot) = slots.get(id) {
            if *slot.borrow() != record {
                slot.send_replace(record);
            }
            return Ok(slot.clone());
        }
        let (slot, _) = watch::channel(record);
        let slot = Arc::new(slot);
        slots.insert(id.into(), slot.clone());
        Ok(slot)
    }
    fn write_record(&self, record: SessionRecord) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.records)?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.records)?;
        serde_json::to_writer(&mut temp, &record)?;
        temp.as_file().sync_all()?;
        temp.persist(self.records.join(format!("{}.json", record.id)))
            .map_err(|error| StoreError::Io(error.error))?;
        let mut slots = self.slots.lock().expect("application records lock");
        if let Some(slot) = slots.get(&record.id) {
            slot.send_replace(record);
        } else {
            let id = record.id.clone();
            let (slot, _) = watch::channel(record);
            slots.insert(id, Arc::new(slot));
        }
        Ok(())
    }
    pub fn update_record(
        &self,
        id: &str,
        update: impl FnOnce(&mut SessionRecord) -> Result<bool, String>,
    ) -> Result<(), String> {
        let _mutation = self
            .mutation
            .lock()
            .expect("application record mutation lock");
        let slot = self.slot(id).map_err(|e| e.to_string())?;
        let mut record = slot.borrow().clone();
        if !update(&mut record)? {
            return Ok(());
        }
        self.write_record(record).map_err(|e| e.to_string())
    }
    pub fn prepare(&self, session: &Session) -> Result<(), StoreError> {
        let _mutation = self
            .mutation
            .lock()
            .expect("application record mutation lock");
        let path = self.records.join(format!("{}.json", session.header.id));
        let resources = if path.exists() {
            self.slot(&session.header.id)?.borrow().resources.clone()
        } else {
            let project = self.base.join(vva_core::storage::project_key(
                &session.header.workspace_root,
            ));
            if project
                .join(format!("{}.jsonl", session.header.id))
                .exists()
            {
                return Err(StoreError::InvalidFormat(format!(
                    "Application record {} is missing",
                    path.display()
                )));
            }
            ResourcePaths {
                scratch: project.join("scratch"),
                tool_outputs: project.join("tool_outputs"),
            }
        };
        std::fs::create_dir_all(&resources.scratch)?;
        std::fs::create_dir_all(&resources.tool_outputs)?;
        self.write_record(session.header.record(resources))
    }
    pub async fn save_header(&self, session: &Session) -> Result<(), StoreError> {
        self.prepare(session)?;
        self.core
            .save_header(&session.to_core())
            .await
            .map_err(|error| {
                StoreError::InvalidFormat(format!(
                    "Application record saved; core session write failed: {error}"
                ))
            })
    }
    pub async fn append_turn(
        &self,
        id: &str,
        turn: &vva_core::model::Turn,
    ) -> Result<(), StoreError> {
        self.core.append_turn(id, turn).await
    }
    pub async fn load_session(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let Some(session) = self.core.load_session(id).await.map_err(migration_hint)? else {
            return Ok(None);
        };
        let record = self.slot(id)?.borrow().clone();
        Ok(Some(Session::from_core(Arc::new(session), &record)))
    }
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        self.core
            .list_sessions()
            .await
            .map_err(migration_hint)?
            .into_iter()
            .map(|summary| {
                let record = self.slot(&summary.id)?.borrow().clone();
                Ok(SessionSummary::from_core(summary, &record))
            })
            .collect()
    }
    pub fn delete_record(&self, id: &str) -> Result<(), StoreError> {
        std::fs::remove_file(self.records.join(format!("{id}.json")))?;
        self.slots
            .lock()
            .expect("application records lock")
            .remove(id);
        Ok(())
    }
    pub async fn delete_session(&self, id: &str) -> Result<(), StoreError> {
        self.core.delete_session(id).await?;
        std::fs::remove_file(self.records.join(format!("{id}.json")))?;
        self.slots
            .lock()
            .expect("application records lock")
            .remove(id);
        Ok(())
    }
    pub async fn ensure_scratch(&self, workspace: &Path) -> Result<PathBuf, StoreError> {
        let sessions = self.core.list_sessions().await.map_err(migration_hint)?;
        for session in sessions {
            if session.workspace_root == workspace {
                let record = self.slot(&session.id)?.borrow().clone();
                let path = record.resources.scratch;
                std::fs::create_dir_all(&path)?;
                return Ok(path);
            }
        }
        let path = self
            .base
            .join(vva_core::storage::project_key(workspace))
            .join("scratch");
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }
}

fn migration_hint(error: StoreError) -> StoreError {
    match error {StoreError::UnsupportedVersion(2)=>StoreError::InvalidFormat("v2 session: stop all writers and run `vva session migrate --source OLD --destination NEW --stopped`; then use --sessions-dir NEW".into()),error=>error}
}
