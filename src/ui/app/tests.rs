use super::*;
use crate::storage::SessionStore;

#[tokio::test]
async fn test_slash_command_model_execution() {
    let temp_dir = std::env::temp_dir().join(format!("vva_app_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("init_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

    // 1. 无参 /model 打开 Models 模态选择框
    app.set_input("/model");
    app.submit_input().await;
    assert_eq!(app.modal, ActiveModal::Models);
    assert!(app.models_modal.is_some());

    // 2. 带参 /model 切换模型
    app.set_input("/model target_model");
    app.submit_input().await;
    let curr = app
        .active_session
        .session
        .read()
        .await
        .current_model
        .clone();
    assert_eq!(curr, "target_model");
    assert!(app.chat_entries.iter().any(|e| match e {
        ChatEntry::SystemNotice { text } => text.contains("Switched model to 'target_model'"),
        _ => false,
    }));

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_slash_command_variant_execution() {
    let temp_dir =
        std::env::temp_dir().join(format!("vva_app_variant_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
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
                        "variants": ["low", "high"]
                    },
                    "model-without-vars": {
                        "name": "Model Without Variants"
                    }
                }
            }
        }
    }"#;
    let app_config = crate::config::parser::parse_config_str(config_str).unwrap();
    let session_mgr = Arc::new(SessionManager::new(
        store,
        Some(app_config),
        temp_dir.clone(),
    ));
    let session = session_mgr
        .create_session("prov/model-with-vars".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr.clone(), session, false).await;

    // 1. 无参 /variant 打开 Variants 模态框，包含 none (default) 与配置的 variants
    app.set_input("/variant");
    app.submit_input().await;
    assert_eq!(app.modal, ActiveModal::Variants);
    assert!(app.variants_modal.is_some());
    let vm = app.variants_modal.as_ref().unwrap();
    assert_eq!(vm.variants.len(), 3);
    assert_eq!(vm.variants[0].id, None);
    assert_eq!(vm.variants[1].id.as_deref(), Some("low"));
    assert_eq!(vm.variants[2].id.as_deref(), Some("high"));

    // 2. 带参 /variant 切换推理档位
    app.set_input("/variant high");
    app.submit_input().await;
    let curr = app
        .active_session
        .session
        .read()
        .await
        .current_variant
        .clone();
    assert_eq!(curr, Some("high".to_string()));
    assert!(app.chat_entries.iter().any(|e| match e {
        ChatEntry::SystemNotice { text } => text.contains("Set reasoning variant to 'high'"),
        _ => false,
    }));

    // 3. /variant none 重置为 None
    app.set_input("/variant none");
    app.submit_input().await;
    let curr_none = app
        .active_session
        .session
        .read()
        .await
        .current_variant
        .clone();
    assert_eq!(curr_none, None);

    // 4. 未配置 variants 的模型，模态框列表为空（不兜底）
    let session_no_vars = session_mgr
        .create_session("prov/model-without-vars".into(), None, None, false)
        .await
        .unwrap();
    let mut app_no_vars = App::new(session_mgr, session_no_vars, false).await;
    app_no_vars.set_input("/variant");
    app_no_vars.submit_input().await;
    assert_eq!(app_no_vars.modal, ActiveModal::Variants);
    let vm_empty = app_no_vars.variants_modal.as_ref().unwrap();
    assert!(vm_empty.variants.is_empty());

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_slash_command_thinking_execution() {
    let temp_dir =
        std::env::temp_dir().join(format!("vva_app_thinking_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("test_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

    assert!(!app.thinking_concise);

    // 执行 /thinking 切换为 concise
    app.set_input("/thinking");
    app.submit_input().await;
    assert!(app.thinking_concise);
    assert!(app.chat_entries.iter().any(|e| match e {
        ChatEntry::SystemNotice { text } =>
            text.contains("Thinking display mode set to concise"),
        _ => false,
    }));

    // 再次执行 /think 切换回 full
    app.set_input("/think");
    app.submit_input().await;
    assert!(!app.thinking_concise);
    assert!(app.chat_entries.iter().any(|e| match e {
        ChatEntry::SystemNotice { text } => text.contains("Thinking display mode set to full"),
        _ => false,
    }));

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_app_scroll_control_and_clamping() {
    let temp_dir =
        std::env::temp_dir().join(format!("vva_app_scroll_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("test_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

    // 初始状态：无滚动条，处于自动吸底模式
    assert_eq!(app.max_scroll, 0);
    assert!(app.auto_scroll);
    app.scroll_up(5);
    assert!(app.auto_scroll, "When max_scroll is 0, scroll_up should not break auto_scroll");

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

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_session_resume_preserves_web_search_state() {
    let temp_dir =
        std::env::temp_dir().join(format!("vva_app_resume_search_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store.clone(), None, temp_dir.clone()));

    // 1. 创建 Session 1 并发送首条消息落盘，初始开启 web_search
    let s1 = session_mgr
        .create_session("model_1".into(), None, Some("Session 1".into()), true)
        .await
        .unwrap();
    session_mgr.submit_prompt(s1.clone(), "init s1").await.unwrap();
    let s1_id = s1.id.clone();

    // 2. 创建 Session 2 并发送首条消息落盘，初始关闭 web_search
    let s2 = session_mgr
        .create_session("model_2".into(), None, Some("Session 2".into()), false)
        .await
        .unwrap();
    session_mgr.submit_prompt(s2.clone(), "init s2").await.unwrap();
    let s2_id = s2.id.clone();

    // 3. 启动 App，以 Session 1 为初始会话
    let mut app = App::new(session_mgr.clone(), s1, false).await;
    // App 继承 Session 1 的 web_search: true
    assert!(app.enable_web_search);

    // 4. 切换到 Session 2
    app.switch_to_session(&s2_id).await.unwrap();
    // App 状态切换为 Session 2 的 web_search: false
    assert!(!app.enable_web_search);

    // 5. 切换回 Session 1
    app.switch_to_session(&s1_id).await.unwrap();
    // 恢复 Session 1 时，状态与上一次保持一致为 true，避免 prompt cache 失效
    assert!(app.enable_web_search);

    // 6. 执行 /web off 显式关闭 Session 1 的网络搜索
    app.set_input("/web off");
    app.submit_input().await;
    assert!(!app.enable_web_search);

    // 校验 Session 1 持久化存储中的 web_search 已同步更新为 false
    let loaded_s1 = store.load_session(&s1_id).await.unwrap().unwrap();
    assert!(!loaded_s1.web_search);

    // 7. 再次执行 /web on 开启 Session 1 的网络搜索
    app.set_input("/web on");
    app.submit_input().await;
    assert!(app.enable_web_search);

    let loaded_s1_again = store.load_session(&s1_id).await.unwrap().unwrap();
    assert!(loaded_s1_again.web_search);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_selection_state_lifecycle() {
    let temp_dir = std::env::temp_dir().join(format!("vva_app_sel_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("init_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

    // 1. 初始状态选区为空
    assert!(app.text_selection.is_none());
    assert!(!app.is_selecting_text);

    // 2. 开始拖选
    app.start_text_selection(2, 5);
    assert!(app.is_selecting_text);
    assert_eq!(
        app.text_selection,
        Some(TextSelection::new(
            TextCoord { line: 2, col: 5 },
            TextCoord { line: 2, col: 5 }
        ))
    );

    // 3. 拖拽移动游标
    app.update_text_selection(3, 10);
    assert_eq!(
        app.text_selection,
        Some(TextSelection::new(
            TextCoord { line: 2, col: 5 },
            TextCoord { line: 3, col: 10 }
        ))
    );

    // 4. 释放鼠标完成拖选
    app.finish_text_selection();
    assert!(!app.is_selecting_text);
    assert!(app.text_selection.is_some());

    // 5. 单击（起止点相同）释放时自动清空选区
    app.start_text_selection(4, 2);
    app.finish_text_selection();
    assert!(!app.is_selecting_text);
    assert!(app.text_selection.is_none());

    // 6. 手动清空选区
    app.start_text_selection(1, 0);
    app.update_text_selection(1, 10);
    app.finish_text_selection();
    assert!(app.text_selection.is_some());
    app.clear_text_selection();
    assert!(app.text_selection.is_none());

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_input_selection_state_and_deletion() {
    let temp_dir = std::env::temp_dir().join(format!("vva_app_input_sel_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("init_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

    // 1. 设置输入内容与初始状态
    app.set_input("Hello, 世界!");
    assert!(app.input.selection.is_none());
    assert!(!app.is_selecting_input);

    // 2. 拖拽选区
    app.start_input_selection(2);
    assert!(app.is_selecting_input);
    app.update_input_selection(7);
    app.finish_input_selection();
    assert!(!app.is_selecting_input);
    assert_eq!(app.input.selected_text(), Some("llo, ".to_string()));

    // 3. Backspace 删除选区
    app.backspace();
    assert_eq!(app.input.as_str(), "He世界!");
    assert!(app.input.selection.is_none());

    // 4. 单击（起止点相同）释放时自动清空选区
    app.start_input_selection(2);
    app.finish_input_selection();
    assert!(!app.is_selecting_input);
    assert!(app.input.selection.is_none());

    // 5. 键入字符串替换选区
    app.set_input("foo bar baz");
    app.start_input_selection(4);
    app.update_input_selection(7);
    app.finish_input_selection();
    app.insert_str("qux");
    assert_eq!(app.input.as_str(), "foo qux baz");
    assert!(app.input.selection.is_none());

    // 6. Delete 键删除选区
    app.start_input_selection(4);
    app.update_input_selection(7);
    app.finish_input_selection();
    app.delete();
    assert_eq!(app.input.as_str(), "foo  baz");
    assert!(app.input.selection.is_none());

    // 7. 手动清空输入选区
    app.start_input_selection(0);
    app.update_input_selection(3);
    app.finish_input_selection();
    assert!(app.input.selection.is_some());
    app.clear_input_selection();
    assert!(app.input.selection.is_none());

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_base64_encode() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"Hello World"), "SGVsbG8gV29ybGQ=");
}

#[tokio::test]
async fn test_paste_multiline_into_input_buffer_preserves_lines() {
    let temp_dir =
        std::env::temp_dir().join(format!("vva_paste_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("test_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

    assert!(app.last_key_input_time.is_none());

    // 模拟粘贴一段多行代码
    let multiline_code = "fn main() {\r\n    println!(\"hello\");\r\n}\n";
    app.insert_str(multiline_code);

    // 验证多行完整保留在输入框中，行数正确，末尾规范化为 \n
    assert_eq!(app.input.as_str(), "fn main() {\n    println!(\"hello\");\n}\n");
    assert_eq!(app.input_line_count(), 4);
    assert!(app.last_key_input_time.is_some());

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_input_time_updated_on_char_insert() {
    let temp_dir =
        std::env::temp_dir().join(format!("vva_time_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("test_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

    assert!(app.last_key_input_time.is_none());
    app.insert_char('a');
    let t1 = app.last_key_input_time;
    assert!(t1.is_some());

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    app.insert_char('b');
    let t2 = app.last_key_input_time;
    assert!(t2 > t1);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_prompt_history_rotation_and_draft_restore() {
    let temp_dir =
        std::env::temp_dir().join(format!("vva_history_test_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
    let session = session_mgr
        .create_session("test_model".into(), None, None, false)
        .await
        .unwrap();
    let mut app = App::new(session_mgr, session, false).await;

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

    let _ = std::fs::remove_dir_all(&temp_dir);
}
