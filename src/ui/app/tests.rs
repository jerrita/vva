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
async fn test_cycle_variant_wraps_through_declared_order() {
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

    // default → none → low → high → 回到 default
    for expected in [Some("none"), Some("low"), Some("high"), None] {
        app.cycle_variant().await;
        assert_eq!(
            app.active_session
                .snapshot()
                .session
                .current_variant
                .as_deref(),
            expected,
            "cycle 应按声明顺序推进到 {expected:?}"
        );
    }

    // 未配置 variants 的模型：无可循环档位，保持原状
    let session_no_vars = app
        .session_mgr
        .create_session("prov/model-without-vars".into(), None, None, false)
        .await
        .unwrap();
    let mut app_no_vars = App::new(app.session_mgr.clone(), session_no_vars).await;
    app_no_vars.cycle_variant().await;
    assert_eq!(
        app_no_vars
            .active_session
            .snapshot()
            .session
            .current_variant,
        None
    );

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_tool_right_click_before_request_completion() {
    use crate::model::{ContentPart, Role, ToolCall, ToolResult};
    use crate::ui::app::ToolExpand;

    let mut ta = create_test_app("test_model", None, None, false).await;
    let app = &mut ta.app;
    for (id, name, args, output, modes) in [
        (
            "shell",
            "Shell",
            r#"{"intent":"show status","command":"printf ready"}"#,
            "ready",
            vec![ToolExpand::Preview, ToolExpand::Hidden],
        ),
        (
            "edit",
            "Edit",
            r#"{"path":"sample.txt","old_string":"before","new_string":"after"}"#,
            "--- sample.txt\n+++ sample.txt\n@@ -1 +1 @@\n-before\n+after",
            vec![ToolExpand::Hidden],
        ),
    ] {
        app.add_part(
            Role::Assistant,
            ContentPart::ToolCall(ToolCall::new(id, name, args)),
        );
        app.add_part(
            Role::Tool,
            ContentPart::ToolResult(ToolResult::success(id, output)),
        );
        app.tool_expand.insert(id.into(), ToolExpand::Full);
        for mode in modes {
            app.refresh_chat_lines(80);
            let hit = app
                .chat_cache
                .tool_hits
                .iter()
                .find(|hit| hit.id == id)
                .unwrap();
            assert!(hit.block_start < hit.block_end);
            let line = hit.block_start;
            assert!(!app.chat_cache.response_hit_at(line).unwrap().collapsible);
            assert!(app.handle_response_right_click(line));
            assert_eq!(app.tool_expand.get(id), Some(&mode));
            assert!(app.response_expand.is_empty());
        }
    }
    app.refresh_chat_lines(80);
    assert!(
        !app.chat
            .lines
            .iter()
            .any(|line| line.to_string().contains("Worked"))
    );

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_completed_nonfinal_responses_collapse_and_expand() {
    let mut ta = create_test_app("test_model", None, None, false).await;
    let app = &mut ta.app;
    let view = std::sync::Arc::make_mut(&mut app.view);
    let session = std::sync::Arc::make_mut(&mut view.session);

    let mut first =
        crate::model::Turn::assistant_turn(vec![crate::model::Message::text("Earlier response")]);
    first.elapsed_ms = Some(1_500);
    session.append_turn(first);
    let mut last =
        crate::model::Turn::assistant_turn(vec![crate::model::Message::text("Latest response")]);
    last.elapsed_ms = Some(500);
    last.worked_ms = Some(2_000);
    let response_id = last.id.clone();
    session.append_turn(last);
    view.revision += 1;

    app.refresh_chat_lines(80);
    let rendered = app
        .chat
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("> Worked 2s"));
    assert!(!rendered.contains("Earlier response"));
    assert!(rendered.contains("Latest response"));

    let line = app
        .chat_cache
        .response_hits
        .iter()
        .find(|hit| hit.id == response_id)
        .unwrap()
        .line_start;
    assert!(app.handle_response_click(line));
    app.refresh_chat_lines(80);
    assert!(
        app.chat
            .lines
            .iter()
            .any(|line| line.to_string().contains("Earlier response"))
    );

    let line = app
        .chat_cache
        .response_hits
        .iter()
        .find(|hit| hit.id == response_id)
        .unwrap()
        .line_start;
    assert!(app.handle_response_right_click(line));
    app.refresh_chat_lines(80);
    assert!(
        app.chat
            .lines
            .iter()
            .any(|line| line.to_string().contains("> Worked 2s"))
    );

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_response_groups_keep_independent_state_across_prompts() {
    use crate::model::{ContentPart, Message, Turn};

    let mut ta = create_test_app("test_model", None, None, false).await;
    let app = &mut ta.app;
    app.thinking_concise = false;
    let mut response_ids = Vec::new();
    for index in 0..2 {
        let view = std::sync::Arc::make_mut(&mut app.view);
        let session = std::sync::Arc::make_mut(&mut view.session);
        session.append_turn(Turn::user_turn(Message::text(format!("Prompt {index}"))));
        let mut response = Turn::assistant_turn(vec![Message::new(vec![
            ContentPart::reasoning(format!("Work {index}"), None),
            ContentPart::text(format!("Reply {index}")),
        ])]);
        response.worked_ms = Some(1_000);
        response_ids.push(response.id.clone());
        session.append_turn(response);
        view.revision += 1;
        app.refresh_chat_lines(80);
    }
    assert_eq!(app.chat_cache.response_hits.len(), 2);
    assert!(app.chat_cache.response_hits.iter().all(|hit| hit.collapsed));

    let line = app.chat_cache.response_hits[0].line_start;
    assert!(app.handle_response_click(line));
    app.refresh_chat_lines(80);

    let view = std::sync::Arc::make_mut(&mut app.view);
    std::sync::Arc::make_mut(&mut view.session)
        .append_turn(Turn::user_turn(Message::text("Next prompt")));
    view.revision += 1;
    app.refresh_chat_lines(80);
    let rendered = app
        .chat
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Work 0"));
    assert!(!rendered.contains("Work 1"));
    assert!(rendered.contains("Reply 0"));
    assert!(rendered.contains("Reply 1"));
    let first = app
        .chat_cache
        .response_hits
        .iter()
        .find(|hit| hit.id == response_ids[0])
        .unwrap();
    assert!(first.collapsible);
    assert!(!first.collapsed);
    let line = first.line_start;
    assert!(app.handle_response_right_click(line));
    app.refresh_chat_lines(80);
    assert!(app.chat_cache.response_hits.iter().all(|hit| hit.collapsed));

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_completed_single_turn_work_collapses_before_final_reply() {
    let mut ta = create_test_app("test_model", None, None, false).await;
    let app = &mut ta.app;
    app.thinking_concise = false;
    let view = std::sync::Arc::make_mut(&mut app.view);
    let session = std::sync::Arc::make_mut(&mut view.session);

    let mut response = crate::model::Turn::assistant_turn(vec![crate::model::Message::new(vec![
        crate::model::ContentPart::reasoning("Searching", None),
        crate::model::ContentPart::WebSearchCall(crate::model::WebSearchCall::completed(
            "search_1",
            "release date",
            Vec::new(),
        )),
        crate::model::ContentPart::reasoning("Reviewing results", None),
        crate::model::ContentPart::text("Final reply"),
    ])]);
    response.elapsed_ms = Some(2_000);
    response.worked_ms = Some(2_000);
    session.append_turn(response);
    view.revision += 1;

    app.refresh_chat_lines(80);
    let rendered = app
        .chat
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("> Worked 2s"));
    assert!(!rendered.contains("Searching"));
    assert!(!rendered.contains("release date"));
    assert!(!rendered.contains("Reviewing results"));
    assert!(rendered.contains("Final reply"));

    let line = app.chat_cache.response_hits[0].line_start;
    assert!(app.handle_response_click(line));
    app.refresh_chat_lines(80);
    let rendered = app
        .chat
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Searching"));
    assert!(rendered.contains("release date"));
    assert!(rendered.contains("Reviewing results"));
    assert!(rendered.contains("Final reply"));

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}

#[tokio::test]
async fn test_completed_response_collapses_text_before_web_search() {
    let mut ta = create_test_app("test_model", None, None, false).await;
    let app = &mut ta.app;
    app.thinking_concise = false;
    let view = std::sync::Arc::make_mut(&mut app.view);
    let session = std::sync::Arc::make_mut(&mut view.session);

    let mut response = crate::model::Turn::assistant_turn(vec![crate::model::Message::new(vec![
        crate::model::ContentPart::reasoning("Searching", None),
        crate::model::ContentPart::text("Final reply"),
        crate::model::ContentPart::WebSearchCall(crate::model::WebSearchCall::completed(
            "search_1",
            "release date",
            Vec::new(),
        )),
        crate::model::ContentPart::reasoning("Reviewing results", None),
        crate::model::ContentPart::WebSearchCall(crate::model::WebSearchCall::completed(
            "search_2",
            "source threshold",
            Vec::new(),
        )),
    ])]);
    response.elapsed_ms = Some(2_000);
    response.worked_ms = Some(2_000);
    session.append_turn(response);
    view.revision += 1;

    app.refresh_chat_lines(80);
    let rendered = app
        .chat
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("> Worked 2s"));
    assert!(!rendered.contains("Searching"));
    assert!(!rendered.contains("release date"));
    assert!(!rendered.contains("Reviewing results"));
    assert!(!rendered.contains("source threshold"));
    assert!(rendered.contains("Final reply"));

    let line = app.chat_cache.response_hits[0].line_start;
    assert!(app.handle_response_click(line));
    app.refresh_chat_lines(80);
    let rendered = app
        .chat
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Searching"));
    assert!(rendered.contains("release date"));
    assert!(rendered.contains("Reviewing results"));
    assert!(rendered.contains("source threshold"));
    assert!(rendered.contains("Final reply"));

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

#[tokio::test]
async fn test_user_message_hits_and_revert_fills_input() {
    let temp_dir = std::env::temp_dir().join(format!("vva_revert_ui_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let mut session = crate::model::Session::with_model(
        &temp_dir,
        Some("Origin".into()),
        "openai/test_model",
        None,
        false,
    );
    session.append_turn(crate::model::Turn::new(
        crate::model::Role::System,
        vec![crate::model::Message::text("sys")],
    ));
    session.append_turn(crate::model::Turn::user_turn(crate::model::Message::text(
        "first",
    )));
    session.append_turn(crate::model::Turn::assistant_turn(vec![
        crate::model::Message::text("a1"),
    ]));
    session.append_turn(crate::model::Turn::user_turn(crate::model::Message::text(
        "second prompt",
    )));
    session.append_turn(crate::model::Turn::assistant_turn(vec![
        crate::model::Message::text("a2"),
    ]));
    let id = session.id.clone();
    store.save_header(&session).await.unwrap();
    let session_mgr = Arc::new(SessionManager::new(
        store,
        crate::ui::test_support::test_config(),
        temp_dir.clone(),
        temp_dir.join("state.json"),
    ));
    let handle = session_mgr.get_or_load(&id).await.unwrap();
    let mut app = App::new(session_mgr, handle).await;

    app.refresh_chat_lines(80);
    assert_eq!(app.chat_cache.user_hits.len(), 2);
    let first_hit = app.chat_cache.user_hits[0];
    app.open_message_action_at_line(first_hit.line_start);
    assert!(matches!(app.modal, ActiveModal::MessageAction(_)));

    app.revert_user_turn(3, "second prompt".into()).await;
    assert!(app.modal.is_none());
    assert_eq!(app.input.as_str(), "second prompt");
    assert_eq!(app.view.session.turns.len(), 3);
    assert_eq!(app.view.session.turns[2].text_content(), "a1");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_fork_user_turn_creates_titled_session() {
    let temp_dir = std::env::temp_dir().join(format!("vva_fork_ui_{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(&temp_dir).unwrap());
    let mut session = crate::model::Session::with_model(
        &temp_dir,
        Some("Origin".into()),
        "openai/test_model",
        Some("high".into()),
        true,
    );
    session.append_turn(crate::model::Turn::new(
        crate::model::Role::System,
        vec![crate::model::Message::text("sys")],
    ));
    session.append_turn(crate::model::Turn::user_turn(crate::model::Message::text(
        "first",
    )));
    session.append_turn(crate::model::Turn::assistant_turn(vec![
        crate::model::Message::text("a1"),
    ]));
    session.append_turn(crate::model::Turn::user_turn(crate::model::Message::text(
        "second",
    )));
    let origin_id = session.id.clone();
    store.save_header(&session).await.unwrap();
    let session_mgr = Arc::new(SessionManager::new(
        store,
        crate::ui::test_support::test_config(),
        temp_dir.clone(),
        temp_dir.join("state.json"),
    ));
    let handle = session_mgr.get_or_load(&origin_id).await.unwrap();
    let mut app = App::new(session_mgr, handle).await;

    app.fork_user_turn(3, "Branched".into()).await;
    assert_ne!(app.active_session.id, origin_id);
    assert_eq!(app.view.session.title.as_deref(), Some("Branched"));
    assert_eq!(app.view.session.turns.len(), 3);
    assert_eq!(app.view.session.current_variant.as_deref(), Some("high"));

    let origin = app.session_mgr.get_or_load(&origin_id).await.unwrap();
    assert_eq!(origin.snapshot().session.turns.len(), 4);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_slash_skill_completion_and_invocation() {
    let mut ta = create_test_app("init_model", None, None, false).await;
    let app = &mut ta.app;
    // 写入真实技能目录：TUI 补全清单与会话任务的发现结果需一致
    let skill_dir = ta.temp_dir.join(".vva/skills/doc-review");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: doc-review\ndescription: Review docs.\n---\n1. Read the doc.",
    )
    .unwrap();
    app.skills = crate::skills::discover_skills(&ta.temp_dir);
    assert!(app.skills.iter().any(|s| s.name == "doc-review"));

    // 1. 前缀匹配出技能候选
    app.set_input("/doc");
    assert!(
        app.matched_candidates
            .iter()
            .any(|c| c.token() == "/doc-review")
    );

    // 2. 高亮技能候选后 Tab 补全为可继续输入任务描述的形态
    let index = app
        .matched_candidates
        .iter()
        .position(|c| c.token() == "/doc-review")
        .unwrap();
    app.selected_command_index = index;
    app.complete_selected_command();
    assert_eq!(app.input.as_str(), "/doc-review ");
    assert!(app.matched_candidates.is_empty());

    // 3. /<skill_name> 提交：user turn 保留原文，技能正文以 reminder turn 注入
    app.set_input("/doc-review check the guide");
    app.submit_input().await;
    // 提交经会话任务异步落盘，轮询等待本条 user turn 出现
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let turns = &app.active_session.snapshot().session.turns;
        if turns.len() >= 2
            && turns[turns.len() - 2].text_content() == "/doc-review check the guide"
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timeout waiting for skill invocation turns, notices={:?}",
            app.notices
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let turns = &app.active_session.snapshot().session.turns;
    let last = turns.len() - 1;
    assert_eq!(
        turns[last - 1].text_content(),
        "/doc-review check the guide"
    );
    let parsed = crate::system::parse_agents_reminder(&turns[last].text_content()).unwrap();
    assert_eq!(parsed.kind, crate::system::AgentsReminderKind::SkillLoaded);
    assert_eq!(parsed.name, "doc-review");
    assert!(parsed.content.contains("1. Read the doc."));

    let _ = std::fs::remove_dir_all(&ta.temp_dir);
}
