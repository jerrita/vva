//! 测试共享基建：以临时 JSONL 存储构造 App 实例，消除跨文件重复的测试样板。

use crate::config::AppConfig;
use crate::harness::SessionManager;
use crate::storage::jsonl::JsonlSessionStore;
use crate::ui::app::App;
use std::path::PathBuf;
use std::sync::Arc;

/// 带临时存储目录的 App；析构时清理目录
pub(crate) struct TestApp {
    pub app: App,
    pub temp_dir: PathBuf,
}

impl std::ops::Deref for TestApp {
    type Target = App;

    fn deref(&self) -> &App {
        &self.app
    }
}

impl std::ops::DerefMut for TestApp {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

impl TestApp {
    /// 测试向临时工作区写入文件后同步重建文件索引
    pub(crate) fn refresh_file_index(&mut self) {
        let (candidates, _) = crate::find::walk_workspace(&self.temp_dir, true);
        self.app.file_index = Some(Arc::new(candidates));
    }
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
    let mut app = App::new(session_mgr, session).await;
    // 同步填充文件索引（后台通道的遍历结果作废），保证测试确定性
    let (candidates, _) = crate::find::walk_workspace(&temp_dir, true);
    app.file_index = Some(Arc::new(candidates));
    app.file_index_rx = None;
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
    /// 提交一行输入（斜杠命令或普通 prompt）
    pub(crate) async fn run(&mut self, input: &str) {
        self.set_input(input);
        self.submit_input().await;
    }

    /// 活跃会话当前的推理档位
    pub(crate) fn current_variant(&self) -> Option<String> {
        self.active_session
            .snapshot()
            .session
            .header
            .current_variant
            .clone()
    }

    pub(crate) fn add_part(&mut self, role: crate::model::Role, part: crate::model::ContentPart) {
        let view = Arc::make_mut(&mut self.view);
        view.revision += 1;
        Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
            role,
            vec![crate::model::Message::new(vec![part])],
        ));
    }
}
