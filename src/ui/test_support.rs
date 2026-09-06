//! 测试共享基建：以临时 JSONL 存储构造 App 实例，消除跨文件重复的测试样板。

use crate::config::AppConfig;
use crate::harness::SessionManager;
use crate::storage::jsonl::JsonlSessionStore;
use crate::ui::app::App;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) struct TestApp {
    pub app: App,
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
    let session_mgr = Arc::new(SessionManager::new(
        store,
        config.unwrap_or_else(test_config),
        temp_dir.clone(),
        temp_dir.join("state.json"),
    ));
    let session = session_mgr
        .create_session(model.into(), variant.map(str::to_string), None, web_search)
        .await
        .unwrap();
    let app = App::new(session_mgr, session).await;
    TestApp { app, temp_dir }
}

pub(crate) fn test_config() -> AppConfig {
    let mut config = AppConfig::default();
    config.settings.default = "openai/test_model".into();
    config.providers.insert(
        "openai".into(),
        crate::config::ProviderConfig {
            api_key: "invalid\nheader".into(),
            ..Default::default()
        },
    );
    config
}

impl App {
    pub(crate) fn add_part(&mut self, role: crate::model::Role, part: crate::model::ContentPart) {
        let view = Arc::make_mut(&mut self.view);
        view.revision += 1;
        Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
            role,
            vec![crate::model::Message::new(vec![part])],
        ));
    }
}
