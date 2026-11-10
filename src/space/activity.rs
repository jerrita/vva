//! 完成回执确认后的 Space 活动记录。
use crate::app::{records::Application, store::AppSessionStore};
pub(crate) fn touch(store: &AppSessionStore, id: &str) -> Result<(), String> {
    store.update_record(id, |record| match &mut record.application {
        Application::Space(space) => {
            space.last_activity = Some(chrono::Utc::now());
            Ok(true)
        }
        Application::Coding => Ok(false),
    })
}
