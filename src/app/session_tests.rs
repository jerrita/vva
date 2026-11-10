use crate::app::VvaSessionManager as SessionManager;
use crate::config::AppConfig;
use crate::config::{ModelConfig, ProviderConfig, model::ModelAnnotation};
use crate::harness::session::handle::Command;
use crate::harness::session::task::*;
use crate::harness::session::tests::*;
use crate::harness::testing::StubBroker;
use crate::harness::*;
use crate::model::EditorKind;
use crate::model::{ContentPart, MediaSource, Message, Role, Session, Turn};
use crate::provider::CompletionOptions;
use crate::storage::SessionStore;
use crate::storage::jsonl::JsonlSessionStore;
use crate::tools::agent::{SessionBroker as _, SubagentContext, SubagentReport};
use crate::tools::background::BackgroundStatus;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

fn manager_for(dir: &Path, config: AppConfig) -> Arc<SessionManager> {
    let store = Arc::new(JsonlSessionStore::new(dir).unwrap());
    Arc::new(SessionManager::new(
        store,
        config,
        dir,
        dir.join("state.json"),
    ))
}

fn provider_config(
    api_key: &str,
    base_url: &str,
    models: Vec<(&str, ModelConfig)>,
) -> ProviderConfig {
    ProviderConfig {
        api_key: api_key.into(),
        base_url: base_url.into(),
        models: models
            .into_iter()
            .map(|(id, model)| (id.to_string(), model))
            .collect(),
        ..Default::default()
    }
}

fn model(name: &str, editor: Option<EditorKind>) -> ModelConfig {
    ModelConfig {
        name: name.into(),
        annotation: editor.map(|editor| ModelAnnotation {
            editor: Some(editor),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// 只含 `openai` 提供商的配置（无自定义模型）
fn openai_config() -> AppConfig {
    let mut config = AppConfig::default();
    config.settings.default = "openai/default_model".into();
    config
        .providers
        .insert("openai".into(), provider_config("test", "", Vec::new()));
    config
}

/// 端点不可达 (`p/m`)：模型请求快速失败，不出本机
fn unreachable_config() -> AppConfig {
    let mut config = AppConfig::default();
    config.settings.default = "p/m".into();
    config.providers.insert(
        "p".into(),
        provider_config("k", "http://127.0.0.1:1", vec![("m", model("M", None))]),
    );
    config
}

// ---- 配置与模型切换 ----

#[tokio::test]
async fn curation_tool_persists_running_session_flag_and_reports_write_failure() {
    let (mut task, view) = event_task();
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(unreachable_config()),
        None,
    );
    let session = Arc::make_mut(&mut task.view.session);
    session.set_model("p/m", None);
    session.header.title = Some("fixture".into());
    session.header.origin = Some(crate::model::Origin {
        channel: "test".into(),
        chat_id: "chat".into(),
    });
    let (commands, mut rx) = mpsc::channel(4);
    task.commands = commands.downgrade();
    let runtime = task.start("fixture", Vec::new()).await.unwrap();
    let tool = runtime.tools.get("SetSessionCuration").unwrap();
    let (_events_tx, mut events_rx) = mpsc::channel(4);
    let before = task.view.session.turns.clone();
    let activity = task.view.session.header.last_activity;
    for enabled in [true, true, false, false, true] {
        let args = serde_json::json!({"enabled": enabled}).to_string();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            task.drive(tool.execute(&args), &mut rx, &mut events_rx),
        )
        .await
        .expect("tool must finish while session is running")
        .unwrap();
        result.unwrap();
        assert_eq!(view.borrow().session.header.curation_requested, enabled);
        let stored = task
            .store
            .load_session(&task.view.session.header.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.header.curation_requested, enabled);
        assert_eq!(stored.turns, before);
        assert_eq!(stored.header.last_activity, activity);
    }
    for args in [
        r#"{}"#,
        r#"{"enabled":"true"}"#,
        r#"{"enabled":false,"session_id":"other"}"#,
    ] {
        assert!(tool.execute(args).await.is_err());
        assert!(task.view.session.header.curation_requested);
    }

    // 后续轮次写入仍保留标记。
    task.execute(engine(done_provider(), runtime.tools), &mut rx)
        .await
        .unwrap();
    task.finish_turn(Ok(TurnOutcome::Completed)).await;
    assert!(
        task.store
            .load_session(&task.view.session.header.id)
            .await
            .unwrap()
            .unwrap()
            .header
            .curation_requested
    );

    // 用普通文件占住存储目录，保证 header 写入失败，内存标记应保持原值。
    let root = task.view.session.header.workspace_root.clone();
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::write(&root, "blocked").unwrap();
    let result = task
        .drive(
            tool.execute(r#"{"enabled":false}"#),
            &mut rx,
            &mut events_rx,
        )
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(view.borrow().session.header.curation_requested);
    std::fs::remove_file(root).unwrap();
}

#[tokio::test]
async fn curation_tool_is_only_registered_for_managed_top_level_topics() {
    for (kind, origin, parent, expected) in [
        (crate::model::SessionKind::Topic, true, None, true),
        (crate::model::SessionKind::Topic, false, None, false),
        (
            crate::model::SessionKind::Topic,
            true,
            Some("sess_parent".to_string()),
            false,
        ),
        (crate::model::SessionKind::Curator, true, None, false),
    ] {
        let (mut task, _view) = event_task();
        task.setup = setup_for(
            &task.view.session,
            task.store.clone(),
            Arc::new(unreachable_config()),
            None,
        );
        let session = Arc::make_mut(&mut task.view.session);
        session.set_model("p/m", None);
        session.header.title = Some("fixture".into());
        session.header.kind = kind;
        session.header.parent = parent;
        session.header.origin = origin.then(|| crate::model::Origin {
            channel: "test".into(),
            chat_id: "chat".into(),
        });
        let engine = task.start("fixture", Vec::new()).await.unwrap();
        assert_eq!(engine.tools.get("SetSessionCuration").is_some(), expected);
        cleanup(&task);
    }
}

#[tokio::test]
async fn model_switch_checks_persisted_media_before_mutating_state() {
    let (mut task, _) = event_task();
    task.setup = setup_for(&task.view.session, task.store.clone(), Arc::new(serde_json::from_value(serde_json::json!({
        "providers": {"test": {"models": {
            "vision": {"name": "Vision", "modalities": {"input": ["text", "image"]}},
            "text": {"name": "Text", "modalities": {"input": ["text"]}},
            "other": {"name": "Other", "type": "anthropic", "modalities": {"input": ["text", "image"]}}
        }}}
    })).unwrap()), None);
    let session = Arc::make_mut(&mut task.view.session);
    session.set_model("test/vision", None);
    session.turns = crate::provider::common::media_tool_session().turns;
    task.store.save_header(&task.view.session).await.unwrap();
    let id = task.view.session.header.id.clone();
    task.view.session = Arc::new(task.store.load_session(&id).await.unwrap().unwrap());
    task.view.persisted = true;
    let before = task.view.session.clone();
    let revision = task.view.revision;
    let error = task
        .configure(Some("test/text".into()), None, Default::default())
        .await
        .unwrap_err();
    assert!(error.contains("does not support image input"));
    assert_eq!(task.view.revision, revision);
    assert_eq!(task.view.session.header.current_model, "test/vision");
    assert!(
        !task
            .view
            .session
            .header
            .workspace_root
            .join("state.json")
            .exists()
    );
    let stored = task.store.load_session(&id).await.unwrap().unwrap();
    assert_eq!(stored.header.current_model, "test/vision");
    task.configure(Some("test/other".into()), None, Default::default())
        .await
        .unwrap();
    assert_eq!(task.view.session.header.current_model, "test/other");
    assert_eq!(task.view.session.turns, before.turns);
    cleanup(&task);
}

// ---- 后台任务与子代理完成注入 ----

#[tokio::test]
async fn task_completion_injects_persisted_user_turn() {
    let echo = if cfg!(windows) {
        "Write-Output done"
    } else {
        "echo done"
    };
    let (mut task, id) = task_with_finished_background(echo).await;

    // 完成状态关联任务身份
    task.queue_task_completion(id.clone()).await;
    assert!(task.view.notices.is_empty());
    assert_eq!(
        task.view.background_tasks[&id].status,
        crate::model::BackgroundTaskStatus::Completed
    );

    task.drain_task_completions().await.unwrap();
    assert!(task.task_completions.is_empty());
    let text = task.view.session.turns.last().unwrap().text_content();
    let reminder = crate::system::parse_agents_reminder(&text).unwrap();
    assert_eq!(
        reminder.kind,
        crate::system::AgentsReminderKind::BackgroundTask
    );
    assert!(text.contains(&id) && text.contains("done"), "{text}");
    let loaded = task
        .store
        .load_session(&task.view.session.header.id)
        .await
        .unwrap()
        .unwrap();
    let snapshot = loaded
        .turns
        .last()
        .unwrap()
        .background_task
        .as_ref()
        .unwrap();
    assert_eq!(
        snapshot.status,
        crate::model::BackgroundTaskStatus::Completed
    );
    assert!(snapshot.output.contains("done"));
    assert!(
        loaded
            .turns
            .last()
            .unwrap()
            .text_content()
            .contains("background-task")
    );
    cleanup(&task);
}

#[tokio::test]
async fn execute_drains_completions_before_next_model_call() {
    let (mut task, id) =
        task_with_finished_background(if cfg!(windows) { "exit 0" } else { "true" }).await;
    // 空闲路径也发布任务完成状态
    task.queue_task_completion(id).await;
    assert!(task.view.notices.is_empty());
    assert_eq!(task.view.background_tasks.len(), 1);

    let (_tx, mut rx) = mpsc::channel(1);
    let finished = task
        .execute(engine(done_provider(), Default::default()), &mut rx)
        .await
        .unwrap();
    assert_eq!(finished, TurnOutcome::Completed);

    let turns = &task.view.session.turns;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, Role::User);
    assert!(crate::system::parse_agents_reminder(&turns[0].text_content()).is_some());
    assert_eq!(turns[1].role, Role::Assistant);
    assert_eq!(turns[1].text_content(), "done");
    cleanup(&task);
}

#[tokio::test]
async fn drain_subagent_completions_injects_final_message_reminder() {
    let (mut task, _view) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    let report = SubagentReport {
        session_id: "sess_kid1".into(),
        agent: "explore".into(),
        title: "[explore] probe".into(),
        turn_seq: 1,
        error: None,
        text: "found 3 matches".into(),
    };
    task.queue_session_completion(report, true).await;
    task.drain_subagent_completions().await.unwrap();

    let text = task.view.session.turns.last().unwrap().text_content();
    assert_eq!(
        crate::system::parse_agents_reminder(&text).unwrap().kind,
        crate::system::AgentsReminderKind::BackgroundTask
    );
    assert!(text.contains("sess_kid1"), "{text}");
    assert!(text.contains("found 3 matches"), "{text}");
    assert!(task.subagent_completions.is_empty());
    task.queue_session_completion(
        SubagentReport {
            session_id: "sess_gone".into(),
            agent: String::new(),
            title: String::new(),
            turn_seq: 0,
            text: String::new(),
            error: Some("Session is closed".into()),
        },
        true,
    )
    .await;
    task.drain_subagent_completions().await.unwrap();
    assert_eq!(task.view.session.turns.len(), 2);
    assert_eq!(
        task.view.session.turns[1]
            .background_task
            .as_ref()
            .unwrap()
            .status,
        crate::model::BackgroundTaskStatus::Failed
    );
    cleanup(&task);
}

// ---- 管理器：模型选择 / 回退 / 分叉 / 编辑器 ----

#[tokio::test]
async fn new_session_uses_last_selected_model() {
    let dir = tmp("last-model");
    let first = manager_for(&dir, openai_config())
        .create_session(
            "openai/first".into(),
            Some("high".into()),
            None,
            crate::model::SessionToggles::default(),
        )
        .await
        .unwrap();
    first
        .configure(
            Some("openai/second".into()),
            Some(Some("low".into())),
            Default::default(),
        )
        .await
        .unwrap();

    let restarted = manager_for(&dir, openai_config());
    let next = restarted
        .open_session(None, None, None, Default::default())
        .await
        .unwrap();
    let snap = next.snapshot();
    assert_eq!(snap.session.header.current_model, "openai/second");
    assert_eq!(snap.session.header.current_variant.as_deref(), Some("low"));

    let explicit = restarted
        .open_session(
            None,
            Some("openai/explicit".into()),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        explicit.snapshot().session.header.current_model,
        "openai/explicit"
    );

    let alpha = restarted
        .create_session(
            "openai/alpha".into(),
            None,
            None,
            crate::model::SessionToggles::default(),
        )
        .await
        .unwrap();
    restarted
        .create_session(
            "openai/beta".into(),
            None,
            None,
            crate::model::SessionToggles::default(),
        )
        .await
        .unwrap();
    restarted.get_or_load(&alpha.id).await.unwrap();
    let after_switch = manager_for(&dir, openai_config())
        .open_session(None, None, None, Default::default())
        .await
        .unwrap();
    assert_eq!(
        after_switch.snapshot().session.header.current_model,
        "openai/alpha"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

async fn seeded_session(dir: &Path) -> (Arc<SessionManager>, Arc<ManagedSession>, String) {
    let mut session = Session::with_model(
        dir,
        Some("Origin".into()),
        "openai/default_model",
        Some("high".into()),
        crate::model::SessionToggles::default(),
    );
    session.append_turn(Turn::new(Role::System, vec![Message::text("sys")]));
    for (user, assistant) in [("first", "a1"), ("second", "a2")] {
        session.append_turn(Turn::user_turn(Message::text(user)));
        session.append_turn(Turn::assistant_turn(vec![Message::text(assistant)]));
    }
    let id = session.header.id.clone();
    let mgr = manager_for(dir, openai_config());
    mgr.store().save_header(&session).await.unwrap();
    let handle = mgr.get_or_load(&id).await.unwrap();
    (mgr, handle, id)
}

#[tokio::test]
async fn revert_truncates_from_user_turn_and_persists() {
    let dir = tmp("revert");
    let (mgr, handle, id) = seeded_session(&dir).await;
    handle.revert(3).await.unwrap();
    let snap = handle.snapshot();
    assert_eq!(snap.session.turns.len(), 3);
    assert_eq!(snap.session.turns[2].text_content(), "a1");
    let loaded = mgr.store().load_session(&id).await.unwrap().unwrap();
    assert_eq!(loaded.turns.len(), 3);
    assert_eq!(loaded.turns[1].text_content(), "first");
    assert!(handle.revert(0).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fork_copies_prefix_and_leaves_origin() {
    let dir = tmp("fork");
    let (mgr, handle, id) = seeded_session(&dir).await;
    let forked = mgr
        .fork_session(&handle, 3, "Branched".into())
        .await
        .unwrap();
    let origin = handle.snapshot();
    let branch = forked.snapshot();
    assert_eq!(origin.session.turns.len(), 5);
    assert_eq!(origin.session.header.title.as_deref(), Some("Origin"));
    assert_eq!(branch.session.turns.len(), 3);
    assert_eq!(branch.session.header.title.as_deref(), Some("Branched"));
    assert_eq!(branch.session.header.current_model, "openai/default_model");
    assert_eq!(
        branch.session.header.current_variant.as_deref(),
        Some("high")
    );
    assert_eq!(branch.session.header.editor, origin.session.header.editor);
    assert_ne!(forked.id, id);
    let loaded = mgr.store().load_session(&forked.id).await.unwrap().unwrap();
    assert_eq!(loaded.turns.len(), 3);
    assert_eq!(loaded.header.title.as_deref(), Some("Branched"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn editor_config(settings_editor: Option<EditorKind>) -> AppConfig {
    let mut config = AppConfig::default();
    config.settings.default = "p/plain".into();
    config.settings.editor = settings_editor;
    config.providers.insert(
        "p".into(),
        provider_config(
            "test",
            "",
            vec![
                ("plain", model("Plain", None)),
                ("hashed", model("Hashed", Some(EditorKind::Hashline))),
                ("patched", model("Patched", Some(EditorKind::Patch))),
            ],
        ),
    );
    config
}

async fn editor_of(mgr: &Arc<SessionManager>, model: &str) -> EditorKind {
    let handle = mgr
        .create_session(
            model.into(),
            None,
            None,
            crate::model::SessionToggles::default(),
        )
        .await
        .unwrap();
    handle.snapshot().session.header.editor
}

#[tokio::test]
async fn session_editor_frozen_at_create() {
    let dir = tmp("editor");
    let mgr = manager_for(&dir, editor_config(None));
    let handle = mgr
        .create_session(
            "p/hashed".into(),
            None,
            None,
            crate::model::SessionToggles::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        handle.snapshot().session.header.editor,
        EditorKind::Hashline
    );
    handle
        .configure(Some("p/patched".into()), None, Default::default())
        .await
        .unwrap();
    let snap = handle.snapshot();
    assert_eq!(snap.session.header.current_model, "p/patched");
    assert_eq!(snap.session.header.editor, EditorKind::Hashline);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn session_editor_falls_back_to_settings_editor() {
    let dir = tmp("editor-default");
    let mgr = manager_for(&dir, editor_config(Some(EditorKind::Patch)));
    assert_eq!(editor_of(&mgr, "p/plain").await, EditorKind::Patch);
    // 模型级 annotation.editor 优先于 settings.editor
    assert_eq!(editor_of(&mgr, "p/hashed").await, EditorKind::Hashline);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn new_session_loads_project_agents_md() {
    let dir = tmp("agents-load");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("AGENTS.md"), "project instructions").unwrap();

    let session = Session::with_model(
        &dir,
        None,
        "openai/test_model",
        None,
        crate::model::SessionToggles { web_search: false },
    );
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let (mut task, _rx) = task_with(session, store);
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(openai_config()),
        None,
    );

    task.start("hello", Vec::new()).await.unwrap();
    let snap = task.view.session;

    // 断言相对结构而非固定下标：存在全局 ~/.vva/AGENTS.md 的机器会多出一个 Global 轮次
    assert_eq!(snap.turns.first().map(|t| t.role), Some(Role::System));
    assert_eq!(snap.turns.last().map(|t| t.role), Some(Role::User));
    assert_eq!(snap.turns.last().unwrap().text_content(), "hello");
    let project = snap.turns[..snap.turns.len() - 1]
        .iter()
        .filter_map(|t| crate::system::parse_agents_reminder(&t.text_content()))
        .find(|r| r.kind == crate::system::AgentsReminderKind::Project)
        .expect("project AGENTS.md injected before user prompt");
    assert!(project.content.contains("project instructions"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn skill_changes_are_announced_before_the_next_user_message() {
    let dir = tmp("skills-changed");
    let skills = dir.join(".vva").join("skills");
    std::fs::create_dir_all(skills.join("alpha")).unwrap();
    let write = |name: &str, desc: &str| {
        std::fs::create_dir_all(skills.join(name)).unwrap();
        std::fs::write(
            skills.join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody\n"),
        )
        .unwrap();
    };
    write("alpha", "Alpha skill.");

    let session = Session::with_model(
        &dir,
        None,
        "openai/test_model",
        None,
        crate::model::SessionToggles { web_search: false },
    );
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let (mut task, _rx) = task_with(session, store);
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(openai_config()),
        None,
    );

    task.start("first", Vec::new()).await.unwrap();
    let kinds = |task: &SessionTask| -> Vec<String> {
        task.view
            .session
            .turns
            .iter()
            .map(|t| t.text_content())
            .filter(|t| t.contains("type=\"skills-changed\""))
            .collect()
    };
    assert!(kinds(&task).is_empty(), "首条消息已含完整清单，不重复提示");

    task.start("second", Vec::new()).await.unwrap();
    assert!(kinds(&task).is_empty(), "无变动不提示");

    write("beta", "Beta skill.");
    task.start("third", Vec::new()).await.unwrap();
    let changed = kinds(&task);
    assert_eq!(changed.len(), 1);
    assert!(changed[0].contains("beta"));
    let turns = &task.view.session.turns;
    let n = turns.len();
    assert!(turns[n - 2].text_content().contains("skills-changed"));
    assert_eq!(turns[n - 1].text_content(), "third", "提示位于用户消息之前");

    task.start("fourth", Vec::new()).await.unwrap();
    assert_eq!(kinds(&task).len(), 1, "同一变动不重复提示");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn submit_expands_at_mentions_into_user_parts() {
    let dir = tmp("mention");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.txt"), "mention body").unwrap();

    let session = Session::with_model(
        &dir,
        None,
        "openai/test_model",
        None,
        crate::model::SessionToggles { web_search: false },
    );
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let (mut task, _rx) = task_with(session, store);
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(openai_config()),
        None,
    );

    task.start("看 @note.txt 与 @missing.txt", Vec::new())
        .await
        .unwrap();
    let snap = task.view.clone();

    // 用户轮次保留原文，其后附加 FileRef 部件；缺失路径转 Notice 不阻断
    let user = snap.session.turns.last().expect("user turn appended");
    assert_eq!(user.text_content(), "看 @note.txt 与 @missing.txt");
    let file_ref = user
        .messages
        .last()
        .unwrap()
        .parts
        .iter()
        .find_map(|p| match p {
            ContentPart::FileRef {
                path,
                content,
                truncated,
            } => Some((path.clone(), content.clone(), *truncated)),
            _ => None,
        })
        .expect("FileRef part appended");
    assert_eq!(file_ref.0, "note.txt");
    assert_eq!(file_ref.1, "mention body");
    assert!(!file_ref.2);
    assert!(snap.notices.iter().any(|n| n.text.contains("missing.txt")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn pending_deliveries_run_as_turns_and_bump_seq() {
    let (mut task, _view) = event_task();
    let seq_rx = task.turn_tx.subscribe();
    // 不可达端点：投递轮次快速失败（不出本机），验证冲刷机制而非补全结果
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(unreachable_config()),
        None,
    );
    Arc::make_mut(&mut task.view.session).set_model("p/m".to_string(), None);
    task.pending_deliveries.push(queued("queued message").0);
    let (_tx, mut rx) = mpsc::channel(1);
    let close = task.run_pending_deliveries(&mut rx).await;
    assert!(!close);

    let turns = &task.view.session.turns;
    assert_eq!(turns.first().map(|turn| turn.role), Some(Role::System));
    let last = turns.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert_eq!(last.text_content(), "queued message");
    assert!(task.pending_deliveries.is_empty());
    // 供应商错误同样完成轮次：seq 递增唤醒等待者
    assert_eq!(seq_rx.borrow().seq, 1);
    cleanup(&task);
}

#[tokio::test]
async fn failed_or_aborted_turn_has_no_old_answer_and_cancels_pending_deliveries() {
    for outcome in [
        Ok(TurnOutcome::Aborted),
        Err("provider failure".to_string()),
    ] {
        let (mut task, _) = event_task();
        Arc::make_mut(&mut task.view.session)
            .append_turn(Turn::assistant_turn(vec![Message::text("earlier answer")]));
        task.turn_start = task.view.session.turns.len();
        let (delivery, receipt) = queued("queued request");
        task.pending_deliveries.push(delivery);
        let (mut steering, steering_receipt) = queued("pending steering");
        steering.mode = InputMode::Steer;
        task.pending_deliveries.push(steering);
        let (mut active, active_receipt) = queued("active steering");
        active.mode = InputMode::Steer;
        task.active_deliveries.push(active);
        assert!(!task.finish_turn(outcome).await);
        assert!(receipt.await.unwrap().is_err());
        assert!(steering_receipt.await.unwrap().is_err());
        assert!(active_receipt.await.unwrap().is_err());
        assert!(task.pending_deliveries.is_empty());
        assert!(task.turn_tx.borrow().text.is_empty());
        assert!(task.turn_tx.borrow().error.is_some());
        let (_, mut commands) = mpsc::channel(1);
        assert!(!task.run_pending_deliveries(&mut commands).await);
        assert_eq!(task.view.session.turns.len(), 1);
        assert_eq!(task.turn_tx.borrow().seq, 1);
        cleanup(&task);
    }
}

// ---- Subagent 调度 ----

/// 不可达端点上的父会话与 broker：子会话轮次快速失败，不出本机
async fn subagent_fixture() -> (
    PathBuf,
    Arc<SessionManager>,
    Arc<ManagedSession>,
    super::session::SessionBrokerHandle,
) {
    let dir = tmp("subagent");
    let manager = manager_for(&dir, unreachable_config());
    let parent = manager
        .create_session(
            "p/m".into(),
            None,
            None,
            crate::model::SessionToggles::default(),
        )
        .await
        .unwrap();
    let broker = super::session::SessionBrokerHandle::new(manager.clone(), parent.id.clone());
    (dir, manager, parent, broker)
}

fn agent_def(name: &str) -> crate::agents::AgentDef {
    crate::agents::AgentDef {
        name: name.into(),
        description: "test".into(),
        model: None,
        tools: vec![crate::tools::names::READ.into()],
        system_prompt: None,
    }
}

/// 子代理派生链路：元数据落盘、注册表可见、wait_turn 返回空文本报告
#[tokio::test]
async fn spawn_subagent_creates_child_session_and_wait_reports() {
    let (dir, manager, parent, broker) = subagent_fixture().await;
    let launch = broker
        .spawn_subagent(
            &SubagentContext::from_session(&parent.snapshot().session),
            &agent_def("explore"),
            "find things",
            "probe",
            false,
        )
        .await
        .unwrap();

    // 子会话已注册并可见
    let report = broker.session_report(&launch.session_id).await.unwrap();
    assert_eq!(report.agent, "explore");
    assert_eq!(report.title, "[explore] probe");
    // 元数据随 Header 持久化
    let loaded = manager
        .store()
        .load_session(&launch.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.header.agent.as_deref(), Some("explore"));
    assert_eq!(loaded.header.parent.as_deref(), Some(parent.id.as_str()));
    assert_eq!(loaded.header.depth, 1);
    assert_eq!(loaded.header.current_model, "p/m");

    // 端点不可达：子会话轮次失败结束，wait_turn 返回空文本
    let report = broker
        .wait_turn(
            &launch.session_id,
            launch.turn_seq,
            Some(Duration::from_secs(15)),
        )
        .await
        .unwrap();
    assert!(report.text.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 深度超限拒绝派生
#[tokio::test]
async fn spawn_subagent_rejects_depth_overflow() {
    let (dir, _manager, parent, broker) = subagent_fixture().await;
    let mut context = SubagentContext::from_session(&parent.snapshot().session);
    context.depth = crate::agents::MAX_SUBAGENT_DEPTH;
    let error = broker
        .spawn_subagent(&context, &agent_def("general"), "prompt", "desc", false)
        .await
        .unwrap_err();
    assert!(error.contains("nesting depth"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 删除会话级联清理子会话：活跃与冷存的派生链递归删盘，无关会话保留
#[tokio::test]
async fn delete_session_cascades_to_child_sessions() {
    let dir = tmp("delete-cascade");
    let manager = manager_for(&dir, AppConfig::default());
    let store = manager.store().clone();

    let session = |title: &str, parent: Option<&Session>| {
        let mut session = Session::with_model(
            &dir,
            Some(title.into()),
            "p/m",
            None,
            crate::model::SessionToggles::default(),
        );
        session.header.parent = parent.map(|p| p.header.id.clone());
        session.header.agent = parent.map(|_| "general".to_string());
        session
    };
    let parent = session("parent", None);
    let live_child = session("child", Some(&parent));
    let cold_child = session("cold", Some(&parent));
    let grandchild = session("grand", Some(&live_child));
    let orphan = session("orphan", None);
    for session in [&parent, &live_child, &cold_child, &grandchild, &orphan] {
        store.save_header(session).await.unwrap();
    }

    let parent_handle = manager.get_or_load(&parent.header.id).await.unwrap();
    let _live_handle = manager.get_or_load(&live_child.header.id).await.unwrap();

    manager.delete_session(&parent_handle).await.unwrap();

    for (session, kept) in [
        (&parent, false),
        (&live_child, false),
        (&cold_child, false),
        (&grandchild, false),
        (&orphan, true),
    ] {
        let loaded = store.load_session(&session.header.id).await.unwrap();
        assert_eq!(loaded.is_some(), kept, "{:?}", session.header.title);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn new_session_appends_preamble_reminders_before_user_prompt() {
    let dir = tmp("preamble");
    std::fs::create_dir_all(&dir).unwrap();
    let session = Session::with_model(
        &dir,
        None,
        "openai/test_model",
        None,
        crate::model::SessionToggles { web_search: false },
    );
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let (mut task, _rx) = task_with(session, store);
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(openai_config()),
        None,
    );
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(openai_config()),
        Some(Arc::new(|_| {
            vec!["<system-reminder type=\"memory\">\nM\n</system-reminder>".into()]
        })),
    );

    task.start("hello", Vec::new()).await.unwrap();
    let turns = &task.view.session.turns;
    let n = turns.len();
    assert_eq!(turns[0].role, Role::System);
    assert!(
        turns[n - 2]
            .text_content()
            .starts_with("<system-reminder type=\"memory\">")
    );
    assert_eq!(turns[n - 1].text_content(), "hello");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn websocket_chain_survives_engine_rebuilds_between_user_turns() {
    use crate::provider::response::websocket_tests::{MockWebSocket, answer};

    let mut server = MockWebSocket::new().await;
    let (mut task, _handle, mut commands) = command_task();
    let mut config = server.config(None);
    let alternate = config.providers["p"].models["m"].clone();
    config
        .providers
        .get_mut("p")
        .unwrap()
        .models
        .insert("alternate".into(), alternate);
    task.setup = setup_for(
        &task.view.session,
        task.store.clone(),
        Arc::new(config),
        None,
    );
    task.store.save_header(&task.view.session).await.unwrap();
    task.append(Turn::user_turn(Message::text("First synthetic turn")))
        .await
        .unwrap();
    let engine = task.build_engine().await.unwrap();
    let provider = engine.provider.clone();
    let (outcome, first) = tokio::join!(task.run_turns(engine, &mut commands), async {
        let request = server.next().await;
        request.reply(answer("resp_first_turn")).await;
        request
    });
    assert_eq!(outcome.unwrap(), TurnOutcome::Completed);
    task.append(Turn::user_turn(Message::text("Second synthetic turn")))
        .await
        .unwrap();
    let engine = task.build_engine().await.unwrap();
    assert!(Arc::ptr_eq(&provider, &engine.provider));
    let (outcome, ()) = tokio::join!(task.run_turns(engine, &mut commands), async {
        let request = server.next().await;
        assert_eq!(request.connection, first.connection);
        assert_eq!(request.body["previous_response_id"], "resp_first_turn");
        assert_eq!(request.body["input"].as_array().unwrap().len(), 1);
        assert_eq!(
            request.body["input"][0]["content"][0]["text"],
            "Second synthetic turn"
        );
        request.reply(answer("resp_second_turn")).await;
    });
    assert_eq!(outcome.unwrap(), TurnOutcome::Completed);

    task.configure(Some("p/alternate".into()), None, Default::default())
        .await
        .unwrap();
    let engine = task.build_engine().await.unwrap();
    assert!(!Arc::ptr_eq(&provider, &engine.provider));
    let (outcome, ()) = tokio::join!(task.run_turns(engine, &mut commands), async {
        let request = server.next().await;
        assert_ne!(request.connection, first.connection);
        assert!(request.body.get("previous_response_id").is_none());
        request.reply(answer("resp_alternate")).await;
    });
    assert_eq!(outcome.unwrap(), TurnOutcome::Completed);
    let root = task.view.session.header.workspace_root.clone();
    drop(task);
    std::fs::remove_dir_all(root).unwrap();
}

/// 本地 Responses SSE 端点记录每次模型请求，便于验证被动唤醒与上下文注入。
async fn completion_endpoint() -> (
    AppConfig,
    mpsc::UnboundedReceiver<serde_json::Value>,
    tokio::task::JoinHandle<()>,
) {
    completion_endpoint_controlled(None).await
}

async fn completion_endpoint_controlled(
    gate: Option<Arc<tokio::sync::Semaphore>>,
) -> (
    AppConfig,
    mpsc::UnboundedReceiver<serde_json::Value>,
    tokio::task::JoinHandle<()>,
) {
    let (requests, rx) = mpsc::unbounded_channel();
    let app = axum::Router::new().fallback(move |axum::Json(request): axum::Json<serde_json::Value>| {
        let requests = requests.clone();
        let gate = gate.clone();
        async move {
            requests.send(request).unwrap();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            let event = serde_json::json!({
                "type": "response.completed", "sequence_number": 1,
                "response": {
                    "id": "resp_fixture", "object": "response", "created_at": 0,
                    "status": "completed", "error": null, "incomplete_details": null,
                    "model": "m", "store": false, "previous_response_id": null,
                    "output": [{"type": "message", "id": "msg_fixture", "status": "completed",
                        "role": "assistant", "content": [{"type": "output_text", "text": "handled", "annotations": []}]}],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2,
                        "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}}
                }
            });
            ([("content-type", "text/event-stream")], format!("data: {event}\n\n"))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = unreachable_config();
    config.providers.get_mut("p").unwrap().base_url =
        format!("http://{}", listener.local_addr().unwrap());
    config
        .providers
        .get_mut("p")
        .unwrap()
        .models
        .get_mut("m")
        .unwrap()
        .annotation = Some(ModelAnnotation {
        websocket: Some(false),
        ..Default::default()
    });
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (config, rx, server)
}

#[tokio::test]
async fn programmatic_steering_continues_the_current_turn_and_preserves_after_delivery() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (config, mut requests, server) =
            completion_endpoint_controlled(Some(gate.clone())).await;
        let (mut task, handle, commands) = command_task();
        task.setup = setup_for(
            &task.view.session,
            task.store.clone(),
            Arc::new(config),
            None,
        );
        task.store.save_header(&task.view.session).await.unwrap();
        let root = task.view.session.header.workspace_root.clone();
        let actor = tokio::spawn(task.run(commands));
        let first_handle = handle.clone();
        let first = tokio::spawn(async move { first_handle.steer_wait("initial request").await });
        requests.recv().await.unwrap();

        let (after, mut after_receipt) = queued("next request");
        handle
            .request(|reply| Command::Deliver {
                delivery: after,
                reply,
            })
            .await
            .unwrap();
        let (mut steering, mut receipt) = queued("adjust the current request");
        steering.mode = InputMode::Steer;
        handle
            .request(|reply| Command::Deliver {
                delivery: steering,
                reply,
            })
            .await
            .unwrap();
        gate.add_permits(1);

        let request = requests.recv().await.unwrap().to_string();
        assert!(request.contains("adjust the current request"));
        assert!(!request.contains("next request"));
        assert_eq!(handle.turn_seq(), 0);
        assert!(matches!(
            receipt.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            after_receipt.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let snapshot = handle.snapshot();
        assert!(
            snapshot
                .session
                .turns
                .iter()
                .any(|turn| turn.steering && turn.text_content() == "adjust the current request")
        );
        assert!(
            snapshot
                .session
                .turns
                .iter()
                .filter(|turn| turn.role == Role::Assistant)
                .all(|turn| turn.worked_ms.is_none())
        );

        gate.add_permits(1);
        let first_report = first.await.unwrap().unwrap();
        let steering_report = receipt.await.unwrap().unwrap();
        assert_eq!(first_report.turn_seq, 1);
        assert_eq!(steering_report.turn_seq, first_report.turn_seq);
        assert_eq!(steering_report.text, "handled");
        let request = requests.recv().await.unwrap().to_string();
        assert!(request.contains("next request"));
        assert_eq!(handle.turn_seq(), 1);
        gate.add_permits(1);
        assert_eq!(after_receipt.await.unwrap().unwrap().turn_seq, 2);

        handle.abort_current().await.unwrap();
        drop(handle);
        actor.await.unwrap();
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    })
    .await
    .expect("programmatic steering timed out");
}

#[tokio::test]
async fn user_queue_steers_before_finalizing_and_runs_after_messages_in_order() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (config, mut requests, server) =
            completion_endpoint_controlled(Some(gate.clone())).await;
        let (mut task, handle, commands) = command_task();
        task.setup = setup_for(
            &task.view.session,
            task.store.clone(),
            Arc::new(config),
            None,
        );
        task.store.save_header(&task.view.session).await.unwrap();
        let root = task.view.session.header.workspace_root.clone();
        let actor = tokio::spawn(task.run(commands));
        handle
            .submit("initial request", vec![], None)
            .await
            .unwrap();
        requests.recv().await.unwrap();
        for prompt in ["follow one", "follow two", "guide one", "guide two"] {
            handle.submit(prompt, vec![], None).await.unwrap();
        }
        let pending = handle.snapshot().pending_inputs.clone();
        for input in &pending[2..] {
            handle
                .set_input_mode(input.id.clone(), InputMode::Steer)
                .await
                .unwrap();
        }
        handle
            .set_input_mode(pending[3].id.clone(), InputMode::After)
            .await
            .unwrap();
        handle
            .set_input_mode(pending[3].id.clone(), InputMode::Steer)
            .await
            .unwrap();
        assert_eq!(handle.snapshot().pending_inputs.len(), 4);
        assert_eq!(
            handle
                .snapshot()
                .session
                .turns
                .iter()
                .filter(|turn| turn.role == Role::User
                    && !turn.text_content().starts_with("<system-reminder"))
                .count(),
            1
        );

        gate.add_permits(1);
        let guided = requests.recv().await.unwrap().to_string();
        assert!(guided.contains("guide one") && guided.contains("guide two"));
        assert!(!guided.contains("follow one") && !guided.contains("follow two"));
        assert_eq!(handle.turn_seq(), 0);
        assert_eq!(handle.snapshot().pending_inputs.len(), 2);
        let snapshot = handle.snapshot();
        let guides: Vec<_> = snapshot
            .session
            .turns
            .iter()
            .filter(|turn| turn.steering)
            .collect();
        assert_eq!(
            guides
                .iter()
                .map(|turn| turn.text_content())
                .collect::<Vec<_>>(),
            ["guide one", "guide two"]
        );
        assert!(
            snapshot
                .session
                .turns
                .iter()
                .filter(|turn| turn.role == Role::Assistant)
                .all(|turn| turn.worked_ms.is_none())
        );
        assert!(handle.take_input(pending[2].id.clone()).await.is_err());
        assert!(
            handle
                .set_input_mode(pending[2].id.clone(), InputMode::After)
                .await
                .is_err()
        );
        assert_eq!(handle.snapshot().pending_inputs.len(), 2);

        gate.add_permits(1);
        let first = requests.recv().await.unwrap().to_string();
        assert!(first.contains("follow one") && !first.contains("follow two"));
        assert_eq!(handle.turn_seq(), 1);
        gate.add_permits(1);
        let second = requests.recv().await.unwrap().to_string();
        assert!(second.contains("follow two"));
        assert_eq!(handle.turn_seq(), 2);
        let mut views = handle.subscribe();
        gate.add_permits(1);
        while handle.is_running() {
            views.changed().await.unwrap();
        }
        assert_eq!(handle.turn_seq(), 3);
        assert!(handle.snapshot().pending_inputs.is_empty());
        assert!(
            handle
                .snapshot()
                .session
                .turns
                .last()
                .unwrap()
                .worked_ms
                .is_some()
        );
        handle.abort_current().await.unwrap();
        drop(handle);
        actor.await.unwrap();
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    })
    .await
    .expect("user queue timed out");
}

#[tokio::test]
async fn stopping_keeps_pending_input_and_take_returns_attachments_once() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (mut config, mut requests, server) = completion_endpoint_controlled(Some(gate)).await;
        config
            .providers
            .get_mut("p")
            .unwrap()
            .models
            .get_mut("m")
            .unwrap()
            .modalities = Some(crate::config::model::ModalitiesConfig {
            input: vec!["text".into(), "image".into()],
            output: vec!["text".into()],
        });
        let (mut task, handle, commands) = command_task();
        task.setup = setup_for(
            &task.view.session,
            task.store.clone(),
            Arc::new(config),
            None,
        );
        task.store.save_header(&task.view.session).await.unwrap();
        let root = task.view.session.header.workspace_root.clone();
        let actor = tokio::spawn(task.run(commands));
        handle
            .submit("initial request", vec![], None)
            .await
            .unwrap();
        requests.recv().await.unwrap();
        let attachment = ContentPart::Image {
            source: MediaSource::Base64 {
                data: "c3ludGhldGlj".into(),
                mime_type: "image/png".into(),
            },
            mime_type: None,
        };
        handle
            .submit("queued input", vec![attachment.clone()], None)
            .await
            .unwrap();
        let id = handle.snapshot().pending_inputs[0].id.clone();
        handle.abort_current().await.unwrap();
        assert!(!handle.is_running());
        assert_eq!(handle.snapshot().pending_inputs.len(), 1);
        let restored = handle.take_input(id.clone()).await.unwrap();
        assert_eq!(restored.prompt, "queued input");
        assert_eq!(restored.attachments, [attachment]);
        assert!(handle.take_input(id).await.is_err());
        assert!(requests.try_recv().is_err());
        drop(handle);
        actor.await.unwrap();
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    })
    .await
    .expect("stopping user queue timed out");
}

#[tokio::test]
async fn idle_session_resumes_for_each_result_and_retains_output_and_delivery() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (config, mut requests, server) = completion_endpoint().await;
        let (mut task, handle, commands) = command_task();
        task.setup = setup_for(
            &task.view.session,
            task.store.clone(),
            Arc::new(config),
            None,
        );
        task.store.save_header(&task.view.session).await.unwrap();
        let root = task.view.session.header.workspace_root.clone();
        let (cli, mut output) = mpsc::channel(64);
        task.cli = Some(cli);
        let (delivery, mut receipt) = queued("original request");
        task.active_deliveries.push(delivery);
        let (first, first_receipt) = oneshot::channel();
        let (second, second_receipt) = oneshot::channel();
        task.watch_session("sess_worker".into(), first_receipt, true);
        task.watch_session("sess_worker".into(), second_receipt, false);
        Arc::make_mut(&mut task.view.session)
            .append_turn(Turn::assistant_turn(vec![Message::text("pending")]));
        task.finish_turn(Ok(TurnOutcome::Completed)).await;
        assert!(handle.has_background_work());
        assert!(!handle.is_running());
        assert!(receipt.try_recv().is_err());
        assert!(requests.try_recv().is_err());
        let mut turns = handle.turn_receiver();
        turns.borrow_and_update();
        let actor = tokio::spawn(task.run(commands));

        first
            .send(Ok(async_report("first result")))
            .unwrap_or_else(|_| panic!("first watcher closed"));
        turns.changed().await.unwrap();
        let first_request = requests.recv().await.unwrap().to_string();
        assert!(first_request.contains("first result"));
        assert!(handle.has_background_work());
        assert!(!turns.borrow().settled);
        assert!(receipt.try_recv().is_err());
        assert!(requests.try_recv().is_err());

        second
            .send(Ok(async_report("second result")))
            .unwrap_or_else(|_| panic!("second watcher closed"));
        turns.changed().await.unwrap();
        assert!(!handle.has_background_work());
        assert!(turns.borrow().settled);
        let final_reply = receipt.await.unwrap().unwrap();
        assert_eq!(final_reply.text, "handled");
        let second_request = requests.recv().await.unwrap().to_string();
        assert!(second_request.contains("second result"));
        // 后续回复保留独立轮次，原后台工具块仍对应首次委派的结果。
        assert_eq!(
            handle.snapshot().background_tasks["sess_worker"].output,
            "first result"
        );
        let mut finished = 0;
        while let Some(event) = output.recv().await {
            if matches!(event, AgentEvent::Finished) {
                finished += 1;
            }
        }
        assert_eq!(finished, 3);
        assert!(requests.try_recv().is_err());
        drop(handle);
        actor.await.unwrap();
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    })
    .await
    .expect("automatic wakeup timed out");
}

#[tokio::test]
async fn stopped_idle_session_persists_results_without_resuming_until_new_input() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (config, mut requests, server) = completion_endpoint().await;
        let (mut task, handle, commands) = command_task();
        task.setup = setup_for(
            &task.view.session,
            task.store.clone(),
            Arc::new(config),
            None,
        );
        task.store.save_header(&task.view.session).await.unwrap();
        let root = task.view.session.header.workspace_root.clone();
        let (result, receipt) = oneshot::channel();
        task.watch_session("sess_worker".into(), receipt, true);
        task.finish_turn(Ok(TurnOutcome::Completed)).await;
        let actor = tokio::spawn(task.run(commands));
        handle.abort_current().await.unwrap();
        result
            .send(Ok(async_report("saved result")))
            .unwrap_or_else(|_| panic!("watcher closed"));
        // 通过视图事件等待持久化，无计时轮询。
        let mut view = handle.subscribe();
        loop {
            if view
                .borrow_and_update()
                .session
                .turns
                .iter()
                .any(|turn| turn.text_content().contains("saved result"))
            {
                break;
            }
            view.changed().await.unwrap();
        }
        assert_eq!(handle.turn_seq(), 1);
        assert!(requests.try_recv().is_err());
        handle.deliver_wait("continue").await.unwrap();
        assert!(
            requests
                .recv()
                .await
                .unwrap()
                .to_string()
                .contains("saved result")
        );
        assert_eq!(handle.turn_seq(), 2);
        drop(handle);
        actor.await.unwrap();
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    })
    .await
    .expect("stopped session did not settle");
}

#[tokio::test]
async fn shell_completion_survives_a_full_command_queue() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut task, handle, mut commands) = command_task();
        task.store.save_header(&task.view.session).await.unwrap();
        let (reply, _) = oneshot::channel();
        handle
            .commands
            .send(Command::Note {
                text: "queued".into(),
                reply,
            })
            .await
            .unwrap();
        let command = if cfg!(windows) { "exit 0" } else { "true" };
        let id = task
            .tasks
            .spawn("fixture", command, &task.view.session.header.workspace_root)
            .unwrap();
        assert_eq!(task.tasks.pending_count(), 1);
        wait_for_task(&task, &id, BackgroundStatus::Exited { code: Some(0) }).await;
        tokio::task::yield_now().await;
        assert!(matches!(commands.recv().await, Some(Command::Note { .. })));
        let Some(Command::TaskFinished { task_id }) = commands.recv().await else {
            panic!("completion missing")
        };
        assert_eq!(task_id, id);
        task.queue_task_completion(task_id).await;
        task.drain_task_completions().await.unwrap();
        assert_eq!(task.tasks.pending_count(), 0);
        cleanup(&task);
    })
    .await
    .expect("shell completion was lost");
}

#[tokio::test]
async fn background_agent_and_followup_reply_automatically_resume_the_owner() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (config, mut requests, server) = completion_endpoint().await;
        let root = tmp("async-agent");
        let manager = manager_for(&root, config);
        let parent = manager
            .create_session(
                "p/m".into(),
                None,
                Some("Parent fixture".into()),
                Default::default(),
            )
            .await
            .unwrap();
        parent.deliver_wait("initial request").await.unwrap();
        let broker = super::session::SessionBrokerHandle::new(manager.clone(), parent.id.clone());
        let baseline = parent.turn_seq();
        let launch = broker
            .spawn_subagent(
                &SubagentContext::from_session(&parent.snapshot().session),
                &agent_def("general"),
                "worker request",
                "worker fixture",
                true,
            )
            .await
            .unwrap();
        assert!(
            parent
                .settled_report(baseline)
                .await
                .unwrap()
                .error
                .is_none()
        );
        let initial = parent.snapshot().background_tasks[&launch.session_id].clone();
        assert_eq!(
            initial.status,
            crate::model::BackgroundTaskStatus::Completed
        );

        let baseline = parent.turn_seq();
        broker
            .deliver(&launch.session_id, "follow-up request")
            .await
            .unwrap();
        assert!(
            parent
                .settled_report(baseline)
                .await
                .unwrap()
                .error
                .is_none()
        );
        let view = parent.snapshot();
        assert_eq!(view.background_tasks[&launch.session_id], initial);
        let reminders: Vec<_> = view
            .session
            .turns
            .iter()
            .filter(|turn| turn.role == Role::User)
            .map(Turn::text_content)
            .filter(|text| text.contains("type=\"background-task\""))
            .collect();
        assert_eq!(reminders.len(), 2);
        assert!(reminders[0].contains("Background agent"));
        assert!(reminders[1].contains("replied (turn 2)"));
        assert_eq!(std::iter::from_fn(|| requests.try_recv().ok()).count(), 5);
        drop(parent);
        drop(broker);
        drop(manager);
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    })
    .await
    .expect("agent result was not delivered automatically");
}

#[tokio::test]
async fn shell_exit_resumes_idle_session_for_success_and_failure() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (config, mut requests, server) = completion_endpoint().await;
        for exit_code in [0, 7] {
            let (mut task, handle, commands) = command_task();
            task.setup = setup_for(
                &task.view.session,
                task.store.clone(),
                Arc::new(config.clone()),
                None,
            );
            task.store.save_header(&task.view.session).await.unwrap();
            let root = task.view.session.header.workspace_root.clone();
            let command = if cfg!(windows) {
                format!("Write-Output fixture; exit {exit_code}")
            } else {
                format!("printf fixture; exit {exit_code}")
            };
            let id = task.tasks.spawn("fixture", &command, &root).unwrap();
            // 退出早于响应结束时，未消费的完成结果也应保留输出通道。
            wait_for_task(
                &task,
                &id,
                BackgroundStatus::Exited {
                    code: Some(exit_code),
                },
            )
            .await;
            let (cli, mut output) = mpsc::channel(64);
            task.cli = Some(cli);
            let (delivery, receipt) = queued("original request");
            task.active_deliveries.push(delivery);
            task.finish_turn(Ok(TurnOutcome::Completed)).await;
            assert!(handle.has_background_work());
            let actor = tokio::spawn(task.run(commands));
            let report = receipt.await.unwrap().unwrap();
            assert_eq!(report.text, "handled");
            let request = requests.recv().await.unwrap().to_string();
            assert!(request.contains(&id));
            assert!(request.contains("fixture"));
            assert!(request.contains(&format!("exited with code {exit_code}")));
            assert_eq!(
                handle.snapshot().background_tasks[&id].exit_code,
                Some(exit_code)
            );
            assert!(!handle.has_background_work());
            while output.recv().await.is_some() {}
            assert!(requests.try_recv().is_err());
            drop(handle);
            actor.await.unwrap();
            let _ = std::fs::remove_dir_all(root);
        }
        server.abort();
    })
    .await
    .expect("shell exit did not resume the session");
}

fn setup_for(
    session: &Session,
    store: Arc<dyn SessionStore>,
    config: Arc<AppConfig>,
    preamble: Option<super::SessionPreamble>,
) -> Arc<super::VvaSessionSetup> {
    let setup = super::VvaSessionSetup::new(
        config,
        store,
        session.header.workspace_root.join("state.json"),
        Arc::new(StubBroker(None)),
        None,
        preamble,
    );
    Arc::new(if session.header.agent_managed() {
        crate::space::session_setup(setup)
    } else {
        setup
    })
}

impl crate::provider::response::websocket_tests::MockWebSocket {
    pub(crate) fn config(&self, websocket: Option<bool>) -> AppConfig {
        let annotation = websocket.map(|enabled| serde_json::json!({"websocket": enabled}));
        serde_json::from_value(serde_json::json!({
            "settings": {"default": "p/m"},
            "providers": {"p": {"base_url": self.url, "api_key": "fixture-key",
                "models": {"m": {"name": "Fixture", "annotation": annotation}}}}
        }))
        .unwrap()
    }
}

#[tokio::test]
async fn websocket_rejection_is_explicit_and_annotation_false_uses_http() {
    use crate::provider::ProviderError;
    use crate::provider::response::websocket_tests::answer;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/v1/responses",
        get(|| async { StatusCode::UPGRADE_REQUIRED })
            .post(|| async { Json(answer("resp_http")["response"].clone()) }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for enabled in [true, false] {
        let config: AppConfig = serde_json::from_value(json!({"providers": {"p": {
            "base_url": format!("http://{address}"), "api_key": "fixture",
            "models": {"m": {"name": "Fixture", "annotation": {"websocket": enabled}}}
        }}}))
        .unwrap();
        let provider = config.resolve_model("p/m", None).unwrap().create_provider();
        let mut session = Session::new(".", None);
        session.append_turn(Turn::user_turn(Message::text("transport fixture")));
        let result = provider
            .complete(
                &session,
                &[],
                &CompletionOptions {
                    model: "m".into(),
                    affinity: Some("fixture".into()),
                    ..Default::default()
                },
            )
            .await;
        if enabled {
            assert!(matches!(
                result,
                Err(ProviderError::ApiError {
                    status: reqwest::StatusCode::UPGRADE_REQUIRED,
                    ..
                })
            ));
        } else {
            assert_eq!(result.unwrap().messages[0].text_content(), "fixture answer");
        }
    }
    server.abort();
}
