use super::*;
use crate::harness::ManagedSession;
use crate::storage::SessionStore;
use crate::ui::test_support::create_test_app;

/// 等待会话后台轮次结束，避免 App::new 的 try_read 与引擎写锁产生调度竞争
async fn wait_until_idle(session: &std::sync::Arc<ManagedSession>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while session.is_running() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn test_slash_command_model_execution() {
    let mut ta = create_test_app("init_model", None, None, false).await;
    let app = &mut ta.app;

    // 1. 无参 /model 打开 Models 模态选择框
    app.set_input("/model");
    app.submit_input().await;
    assert!(matches!(app.modal, ActiveModal::Models(_)));

    // 2. 带参 /model 切换模型
    app.set_input("/model target_model");
    app.submit_input().await;
    let curr = app.active_session.snapshot().session.current_model.clone();
    assert_eq!(curr, "openai/target_model");

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_clear_deletes_current_session_and_creates_new_one() {
    let mut ta = create_test_app("init_model", Some("high"), None, true).await;
    let app = &mut ta.app;
    let previous_id = app.active_session.id.clone();

    app.set_input("/clear");
    app.submit_input().await;

    assert_ne!(app.active_session.id, previous_id);
    assert!(app.view.session.turns.is_empty());
    assert!(app.active_session.snapshot().session.web_search);
    assert_eq!(
        app.active_session
            .snapshot()
            .session
            .current_variant
            .as_deref(),
        Some("high")
    );
    assert!(app.session_mgr.get_or_load(&previous_id).await.is_err());

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_new_creates_session_and_keeps_current_one() {
    let mut ta = create_test_app("init_model", Some("high"), None, true).await;
    let app = &mut ta.app;
    let previous_id = app.active_session.id.clone();

    app.set_input("/new");
    app.submit_input().await;

    assert_ne!(app.active_session.id, previous_id);
    assert!(app.view.session.turns.is_empty());
    assert!(app.active_session.snapshot().session.web_search);
    assert_eq!(
        app.active_session
            .snapshot()
            .session
            .current_variant
            .as_deref(),
        Some("high")
    );
    assert_eq!(
        app.session_mgr.get_or_load(&previous_id).await.unwrap().id,
        previous_id
    );

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_slash_command_variant_execution() {
    let config_str = r#"{
        "providers": {
            "prov": {
                "type": "response",
                "base_url": "https://api.openai.com/v1",
                "api_key": "test_key",
                "models": {
                    "model-with-vars": {
                        "name": "Model With Variants",
                        "reasoning": true,
                        "variants": ["none", "low", "high"]
                    },
                    "model-without-vars": {
                        "name": "Model Without Variants"
                    }
                }
            }
        }
    }"#;
    let app_config = crate::config::parser::parse_config_str(config_str).unwrap();
    let mut ta = create_test_app("prov/model-with-vars", None, Some(app_config), false).await;
    let app = &mut ta.app;

    // 1. 无参 /variant 打开 Variants 模态框，包含 default 与配置的 variants
    app.set_input("/variant");
    app.submit_input().await;
    let ActiveModal::Variants(vm) = &app.modal else {
        panic!("expected variants modal");
    };
    assert_eq!(vm.variants.len(), 4);
    assert_eq!(vm.variants[0].id, None);
    assert_eq!(vm.variants[1].id.as_deref(), Some("none"));
    assert_eq!(vm.variants[2].id.as_deref(), Some("low"));
    assert_eq!(vm.variants[3].id.as_deref(), Some("high"));

    // 2. 带参 /variant 切换推理档位
    app.set_input("/variant high");
    app.submit_input().await;
    let curr = app
        .active_session
        .snapshot()
        .session
        .current_variant
        .clone();
    assert_eq!(curr, Some("high".to_string()));

    // 3. /variant none 显式禁用推理
    app.set_input("/variant none");
    app.submit_input().await;
    let curr_none = app
        .active_session
        .snapshot()
        .session
        .current_variant
        .clone();
    assert_eq!(curr_none.as_deref(), Some("none"));

    // 4. /variant default 不发送推理档位
    app.set_input("/variant default");
    app.submit_input().await;
    assert_eq!(app.active_session.snapshot().session.current_variant, None);

    // 5. 未配置 variants 的模型，模态框列表为空（不兜底）
    let session_no_vars = app
        .session_mgr
        .create_session("prov/model-without-vars".into(), None, None, false)
        .await
        .unwrap();
    let mut app_no_vars = App::new(app.session_mgr.clone(), session_no_vars).await;
    assert!(
        app.session_mgr
            .app_config()
            .resolve_model("prov/model-without-vars", Some("none"))
            .is_err()
    );
    app_no_vars.set_input("/variant");
    app_no_vars.submit_input().await;
    let ActiveModal::Variants(vm_empty) = &app_no_vars.modal else {
        panic!("expected variants modal");
    };
    assert!(vm_empty.variants.is_empty());

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_app_scroll_control_and_clamping() {
    let mut ta = create_test_app("test_model", None, None, false).await;
    let app = &mut ta.app;

    // 初始状态：无滚动条，处于自动吸底模式
    assert_eq!(app.max_scroll, 0);
    assert!(app.auto_scroll);
    app.scroll_up(5);
    assert!(
        app.auto_scroll,
        "When max_scroll is 0, scroll_up should not break auto_scroll"
    );

    // 视口更新：内容超出，总可滚动 50 行
    app.update_viewport(50, 20);
    assert_eq!(app.scroll_offset, 50);
    assert!(app.auto_scroll);

    // 向上滚动 5 行
    app.scroll_up(5);
    assert!(!app.auto_scroll);
    assert_eq!(app.scroll_offset, 45);

    // 向上滚动超过顶部时在 0 处截断
    app.scroll_up(100);
    assert_eq!(app.scroll_offset, 0);

    // 向下滚动 10 行
    app.scroll_down(10);
    assert_eq!(app.scroll_offset, 10);
    assert!(!app.auto_scroll);

    // 向下滚动超过最大可滚动行，恢复吸底状态并精确 clamp 于 max_scroll
    app.scroll_down(100);
    assert_eq!(app.scroll_offset, 50);
    assert!(app.auto_scroll);

    // 一键到顶
    app.scroll_to_top();
    assert_eq!(app.scroll_offset, 0);
    assert!(!app.auto_scroll);

    // 一键到底
    app.scroll_to_bottom();
    assert_eq!(app.scroll_offset, 50);
    assert!(app.auto_scroll);

    // 滚动条比例点击与拖拽测试
    app.scroll_to_ratio(0.0);
    assert_eq!(app.scroll_offset, 0);
    assert!(!app.auto_scroll);

    app.scroll_to_ratio(0.5);
    assert_eq!(app.scroll_offset, 25);
    assert!(!app.auto_scroll);

    app.scroll_to_ratio(1.0);
    assert_eq!(app.scroll_offset, 50);
    assert!(app.auto_scroll);

    // 越界拖拽安全测试
    app.scroll_to_ratio(-0.5);
    assert_eq!(app.scroll_offset, 0);

    app.scroll_to_ratio(2.0);
    assert_eq!(app.scroll_offset, 50);
    assert!(app.auto_scroll);

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_session_resume_preserves_web_search_state() {
    let temp_dir = std::env::temp_dir().join(format!(
        "vva_app_resume_search_test_{}",
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(
        store.clone(),
        crate::ui::test_support::test_config(),
        temp_dir.clone(),
        temp_dir.join("state.json"),
    ));

    // 1. 创建 Session 1 并发送首条消息落盘，初始开启 web_search
    let s1 = session_mgr
        .create_session("model_1".into(), None, Some("Session 1".into()), true)
        .await
        .unwrap();
    session_mgr
        .submit_prompt(s1.clone(), "init s1")
        .await
        .unwrap();
    let s1_id = s1.id.clone();

    // 2. 创建 Session 2 并发送首条消息落盘，初始关闭 web_search
    let s2 = session_mgr
        .create_session("model_2".into(), None, Some("Session 2".into()), false)
        .await
        .unwrap();
    session_mgr
        .submit_prompt(s2.clone(), "init s2")
        .await
        .unwrap();
    let s2_id = s2.id.clone();

    // 等待两个会话的后台轮次结束，再构造 App 读取会话状态
    wait_until_idle(&s1).await;
    wait_until_idle(&s2).await;

    // 3. 启动 App，以 Session 1 为初始会话
    let mut app = App::new(session_mgr.clone(), s1).await;
    // App 继承 Session 1 的 web_search: true
    assert!(app.view.session.web_search);

    // 4. 切换到 Session 2
    app.switch_to_session(&s2_id).await.unwrap();
    // App 状态切换为 Session 2 的 web_search: false
    assert!(!app.view.session.web_search);

    // 5. 切换回 Session 1
    app.switch_to_session(&s1_id).await.unwrap();
    // 恢复 Session 1 时，状态与上一次保持一致为 true，避免 prompt cache 失效
    assert!(app.view.session.web_search);

    // 6. 执行 /web off 显式关闭 Session 1 的网络搜索
    app.set_input("/web off");
    app.submit_input().await;
    assert!(!app.view.session.web_search);

    // 校验 Session 1 持久化存储中的 web_search 已同步更新为 false
    let loaded_s1 = store.load_session(&s1_id).await.unwrap().unwrap();
    assert!(!loaded_s1.web_search);

    // 7. 再次执行 /web on 开启 Session 1 的网络搜索
    app.set_input("/web on");
    app.submit_input().await;
    assert!(app.view.session.web_search);

    let loaded_s1_again = store.load_session(&s1_id).await.unwrap().unwrap();
    assert!(loaded_s1_again.web_search);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_prompt_history_rotation_and_draft_restore() {
    let mut ta = create_test_app("test_model", None, None, false).await;
    let app = &mut ta.app;

    // 1. 提交两次 Prompt
    app.set_input("first prompt");
    app.submit_input().await;

    app.set_input("second prompt");
    app.submit_input().await;

    assert_eq!(app.prompt_history.len(), 2);
    assert_eq!(app.prompt_history[0], "first prompt");
    assert_eq!(app.prompt_history[1], "second prompt");

    // 2. 当前输入半句草稿
    app.set_input("my draft");

    // 3. 按 Up：轮换到上一条历史 (second prompt)，且当前草稿保存
    app.history_previous();
    assert_eq!(app.input.as_str(), "second prompt");
    assert_eq!(app.saved_draft, "my draft");

    // 4. 再按 Up：轮换到更早的历史 (first prompt)
    app.history_previous();
    assert_eq!(app.input.as_str(), "first prompt");

    // 5. 按 Down：回到 second prompt
    app.history_next();
    assert_eq!(app.input.as_str(), "second prompt");

    // 6. 再按 Down：超出历史范围，恢复草稿 "my draft"
    app.history_next();
    assert_eq!(app.input.as_str(), "my draft");
    assert!(app.history_index.is_none());

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}
