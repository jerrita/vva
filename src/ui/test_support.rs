//! 测试共享基建：以临时 JSONL 存储构造 App 实例，消除跨文件重复的测试样板。

use crate::config::AppConfig;
use crate::storage::jsonl::JsonlSessionStore;
use crate::ui::app::App;
use crate::ui::session_manager::SessionManager;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) struct TestApp {
    pub app: App,
    pub store: Arc<JsonlSessionStore>,
    pub temp_dir: PathBuf,
}

pub(crate) async fn create_test_app(
    model: &str,
    variant: Option<&str>,
    config: Option<AppConfig>,
    web_search: bool,
) -> TestApp {
    let temp_dir = std::env::temp_dir().join(format!("vva_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store.clone(), config, temp_dir.clone()));
    let session = session_mgr
        .create_session(model.into(), variant.map(str::to_string), None, web_search)
        .await
        .unwrap();
    let app = App::new(session_mgr, session, web_search).await;
    TestApp {
        app,
        store,
        temp_dir,
    }
}
