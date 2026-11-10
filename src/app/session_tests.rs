use crate::app::VvaSessionManager as SessionManager;
use crate::app::handle::ManagedSession;
use crate::app::model::Session;
use crate::app::store::AppSessionStore as SessionStore;
use crate::app::store::AppSessionStore as JsonlSessionStore;
use crate::config::AppConfig;
use crate::config::{ModelConfig, ProviderConfig, model::ModelAnnotation};
use crate::test_support::broker::StubBroker;
use crate::test_support::session::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use vva_core::harness::*;
use vva_core::model::ContentPart;
use vva_core::model::EditorKind;
use vva_core::model::Message;
use vva_core::model::Role;
use vva_core::model::Turn;
use vva_core::provider::CompletionOptions;
use vva_core::tools::agent::SessionBroker as _;
use vva_core::tools::agent::SubagentContext;

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
    let root = tmp("curation-record");
    let store = Arc::new(JsonlSessionStore::new(&root).unwrap());
    let mut session = Session::with_model(
        &root,
        Some("fixture".into()),
        "p/m",
        None,
        Default::default(),
    );
    session.header.origin = Some(crate::app::records::Origin {
        channel: "test".into(),
        chat_id: "chat".into(),
    });
    session.append_turn(Turn::user_turn(Message::text("retained history")));
    store.save_header(&session).await.unwrap();
    let setup = setup_for(
        &session,
        store.clone(),
        Arc::new(unreachable_config()),
        None,
    );
    let plan = setup.plan(&session.to_core(), &services()).await.unwrap();
    let tool = plan.tools.get("SetSessionCuration").unwrap();
    let before = session.turns.clone();
    let activity = store.slot(&session.header.id).unwrap().borrow().clone();
    for enabled in [true, true, false, false, true] {
        tool.execute(&serde_json::json!({"enabled":enabled}).to_string())
            .await
            .unwrap();
        let loaded = store
            .load_session(&session.header.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.header.curation_requested, enabled);
        assert_eq!(loaded.turns, before);
        assert_eq!(loaded.header.last_activity, session.header.last_activity);
    }
    for args in [
        r#"{}"#,
        r#"{"enabled":"true"}"#,
        r#"{"enabled":false,"session_id":"other"}"#,
    ] {
        assert!(tool.execute(args).await.is_err());
    }
    let record = store.slot(&session.header.id).unwrap();
    std::fs::remove_dir_all(&store.records).unwrap();
    std::fs::write(&store.records, "blocked").unwrap();
    assert!(tool.execute(r#"{"enabled":false}"#).await.is_err());
    let crate::app::records::Application::Space(space) = &record.borrow().application else {
        panic!("expected Space record")
    };
    assert!(space.curation_requested);
    assert_eq!(activity.coding, record.borrow().coding);
    std::fs::remove_file(&store.records).unwrap();
    let _ = std::fs::remove_dir_all(root);
}
#[tokio::test]
async fn curation_tool_is_only_registered_for_managed_top_level_topics() {
    for (kind, origin, parent, expected) in [
        (crate::app::records::SessionKind::Topic, true, None, true),
        (crate::app::records::SessionKind::Topic, false, None, false),
        (
            crate::app::records::SessionKind::Topic,
            true,
            Some("sess_parent".to_string()),
            false,
        ),
        (crate::app::records::SessionKind::Curator, true, None, false),
    ] {
        let root = tmp("curation-registration");
        let store = Arc::new(JsonlSessionStore::new(&root).unwrap());
        let mut session = Session::with_model(
            &root,
            Some("fixture".into()),
            "p/m",
            None,
            Default::default(),
        );
        session.header.kind = kind;
        session.header.parent = parent;
        session.header.origin = origin.then(|| crate::app::records::Origin {
            channel: "test".into(),
            chat_id: "chat".into(),
        });
        let setup = setup_for(&session, store, Arc::new(unreachable_config()), None);
        let plan = setup.plan(&session.to_core(), &services()).await.unwrap();
        assert_eq!(plan.tools.get("SetSessionCuration").is_some(), expected);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn model_switch_checks_persisted_media_before_mutating_state() {
    let root = tmp("model-media");
    let config = serde_json::from_value(serde_json::json!({"providers":{"test":{"models":{
        "vision":{"name":"Vision","modalities":{"input":["text","image"]}},
        "text":{"name":"Text","modalities":{"input":["text"]}},
        "other":{"name":"Other","type":"anthropic","modalities":{"input":["text","image"]}}
    }}}}))
    .unwrap();
    let manager = manager_for(&root, config);
    let mut session = Session::with_model(&root, None, "test/vision", None, Default::default());
    session.turns = crate::test_support::media::media_tool_session().turns;
    manager.store().save_header(&session).await.unwrap();
    let handle = manager.live_or_load(&session.header.id).await.unwrap();
    let before = handle.snapshot();
    let error = handle
        .configure(Some("test/text".into()), None, Default::default())
        .await
        .unwrap_err();
    assert!(error.contains("does not support image input"));
    assert_eq!(handle.snapshot().revision, before.revision);
    assert!(!root.join("state.json").exists());
    assert_eq!(
        manager
            .store()
            .load_session(&handle.id)
            .await
            .unwrap()
            .unwrap()
            .header
            .current_model,
        "test/vision"
    );
    handle
        .configure(Some("test/other".into()), None, Default::default())
        .await
        .unwrap();
    assert_eq!(handle.snapshot().session.header.current_model, "test/other");
    assert_eq!(handle.snapshot().session.turns, before.session.turns);
    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn new_session_uses_last_selected_model() {
    let dir = tmp("last-model");
    let first = manager_for(&dir, openai_config())
        .create_session(
            "openai/first".into(),
            Some("high".into()),
            None,
            crate::app::records::SessionToggles::default(),
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
            crate::app::records::SessionToggles::default(),
        )
        .await
        .unwrap();
    restarted
        .create_session(
            "openai/beta".into(),
            None,
            None,
            crate::app::records::SessionToggles::default(),
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
        crate::app::records::SessionToggles::default(),
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
            crate::app::records::SessionToggles::default(),
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
            crate::app::records::SessionToggles::default(),
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

    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let manager = SessionManager::new(store, unreachable_config(), &dir, dir.join("state.json"));
    let manager = Arc::new(manager);
    let handle = manager
        .create_session(
            "p/m".into(),
            None,
            None,
            crate::app::records::SessionToggles { web_search: false },
        )
        .await
        .unwrap();
    assert!(handle.deliver_wait("hello").await.is_err());
    let snap = handle.snapshot().session.clone();

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

    manager.shutdown().await.unwrap();
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

    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let manager = SessionManager::new(store, unreachable_config(), &dir, dir.join("state.json"));
    let manager = Arc::new(manager);
    let handle = manager
        .create_session(
            "p/m".into(),
            None,
            None,
            crate::app::records::SessionToggles { web_search: false },
        )
        .await
        .unwrap();
    assert!(handle.deliver_wait("first").await.is_err());
    let kinds = |handle: &ManagedSession| -> Vec<String> {
        handle
            .snapshot()
            .session
            .turns
            .iter()
            .map(|t| t.text_content())
            .filter(|t| t.contains("type=\"skills-changed\""))
            .collect()
    };
    assert!(
        kinds(&handle).is_empty(),
        "首条消息已含完整清单，不重复提示"
    );

    assert!(handle.deliver_wait("second").await.is_err());
    assert!(kinds(&handle).is_empty(), "无变动不提示");

    write("beta", "Beta skill.");
    assert!(handle.deliver_wait("third").await.is_err());
    let changed = kinds(&handle);
    assert_eq!(changed.len(), 1);
    assert!(changed[0].contains("beta"));
    let snapshot = handle.snapshot();
    let turns = &snapshot.session.turns;
    let n = turns.len();
    assert!(turns[n - 2].text_content().contains("skills-changed"));
    assert_eq!(turns[n - 1].text_content(), "third", "提示位于用户消息之前");

    assert!(handle.deliver_wait("fourth").await.is_err());
    assert_eq!(kinds(&handle).len(), 1, "同一变动不重复提示");

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn submit_expands_at_mentions_into_user_parts() {
    let dir = tmp("mention");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.txt"), "mention body").unwrap();

    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let manager = SessionManager::new(store, unreachable_config(), &dir, dir.join("state.json"));
    let manager = Arc::new(manager);
    let handle = manager
        .create_session(
            "p/m".into(),
            None,
            None,
            crate::app::records::SessionToggles { web_search: false },
        )
        .await
        .unwrap();
    assert!(
        handle
            .deliver_wait("看 @note.txt 与 @missing.txt")
            .await
            .is_err()
    );
    let snap = handle.snapshot();

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

    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

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
            crate::app::records::SessionToggles::default(),
        )
        .await
        .unwrap();
    let broker = super::session::SessionBrokerHandle::new(manager.clone(), parent.id.clone());
    (dir, manager, parent, broker)
}

fn agent_def(name: &str) -> vva_core::agents::AgentDef {
    vva_core::agents::AgentDef {
        name: name.into(),
        description: "test".into(),
        model: None,
        tools: vec![vva_core::tools::names::READ.into()],
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
    context.depth = vva_core::agents::MAX_SUBAGENT_DEPTH;
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
            crate::app::records::SessionToggles::default(),
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
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let manager = SessionManager::new(store, unreachable_config(), &dir, dir.join("state.json"));
    let manager = manager.with_preamble(Arc::new(|_| {
        vec!["<system-reminder type=\"memory\">\nM\n</system-reminder>".into()]
    }));
    let manager = Arc::new(manager);
    let handle = manager
        .create_session(
            "p/m".into(),
            None,
            None,
            crate::app::records::SessionToggles { web_search: false },
        )
        .await
        .unwrap();
    assert!(handle.deliver_wait("hello").await.is_err());
    let snapshot = handle.snapshot();
    let turns = &snapshot.session.turns;
    let n = turns.len();
    assert_eq!(turns[0].role, Role::System);
    assert!(
        turns[n - 2]
            .text_content()
            .starts_with("<system-reminder type=\"memory\">")
    );
    assert_eq!(turns[n - 1].text_content(), "hello");
    manager.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn websocket_chain_survives_engine_rebuilds_between_user_turns() {
    use crate::test_support::websocket::{MockWebSocket, answer};
    let mut server = MockWebSocket::new().await;
    let root = tmp("websocket-rebuild");
    let mut config = server.config(None);
    let alternate = config.providers["p"].models["m"].clone();
    config
        .providers
        .get_mut("p")
        .unwrap()
        .models
        .insert("alternate".into(), alternate);
    let manager = manager_for(&root, config);
    let handle = manager
        .create_session(
            "p/m".into(),
            None,
            Some("Websocket fixture".into()),
            Default::default(),
        )
        .await
        .unwrap();
    let (outcome, first) = tokio::join!(handle.deliver_wait("First synthetic turn"), async {
        let request = server.next().await;
        request.reply(answer("resp_first_turn")).await;
        request
    });
    outcome.unwrap();
    let (outcome, ()) = tokio::join!(handle.deliver_wait("Second synthetic turn"), async {
        let request = server.next().await;
        assert_eq!(request.connection, first.connection);
        assert_eq!(request.body["previous_response_id"], "resp_first_turn");
        let input = request.body["input"].to_string();
        assert!(input.contains("Second synthetic turn"));
        assert!(!input.contains("First synthetic turn"));
        request.reply(answer("resp_second_turn")).await;
    });
    outcome.unwrap();
    handle
        .configure(Some("p/alternate".into()), None, Default::default())
        .await
        .unwrap();
    let (outcome, ()) = tokio::join!(handle.deliver_wait("Alternate"), async {
        let request = server.next().await;
        assert_ne!(request.connection, first.connection);
        assert!(request.body.get("previous_response_id").is_none());
        request.reply(answer("resp_alternate")).await;
    });
    outcome.unwrap();
    manager.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

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
            vva_core::model::BackgroundTaskStatus::Completed
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

fn services() -> SessionServices {
    SessionServices {
        hashline: vva_core::tools::hashline::HashlineSession::new(),
        ask_user_requests: mpsc::channel(1).0,
        background: Arc::new(vva_core::tools::background::BackgroundTasks::new(Arc::new(
            |_| {},
        ))),
        shell_outputs: Arc::default(),
    }
}
fn setup_for(
    session: &Session,
    store: Arc<SessionStore>,
    config: Arc<AppConfig>,
    preamble: Option<super::SessionPreamble>,
) -> Arc<super::VvaSessionSetup> {
    store.prepare(session).unwrap();
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

impl crate::test_support::websocket::MockWebSocket {
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
    use crate::test_support::websocket::answer;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;
    use vva_core::provider::ProviderError;

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
