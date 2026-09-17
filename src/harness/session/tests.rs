use super::task::*;
use super::*;
use crate::model::{Annotation, SearchStatus, WebSearchAction, WebSearchCall, WebSearchSource};
use crate::storage::jsonl::JsonlSessionStore;
use std::path::Path;

fn event_task() -> (SessionTask, watch::Receiver<Arc<SessionView>>) {
    let workspace = std::env::temp_dir().join(format!("vva-events-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(JsonlSessionStore::new(&workspace).unwrap());
    let view = SessionView {
        revision: 0,
        session: Arc::new(Session::new(workspace.clone(), None)),
        draft: Vec::new(),
        notices: Vec::new(),
        state: RunState::Idle,
        persisted: false,
        pending_user_question: None,
    };
    let (view_tx, view_rx) = watch::channel(Arc::new(view.clone()));
    let (commands, _rx) = mpsc::channel(1);
    let task = SessionTask {
        view,
        view_tx,
        store,
        config: Arc::new(AppConfig::default()),
        hashline: crate::tools::hashline::HashlineSession::new(),
        state_path: workspace.join("state.json"),
        commands: commands.downgrade(),
        dirty: false,
        cli: None,
        stop_request: None,
        notified: HashSet::new(),
        title_task: None,
        text_parts: HashMap::new(),
        ask_user_tx: mpsc::channel(1).0,
        ask_user_rx: mpsc::channel(1).1,
        pending_user_question: None,
    };
    (task, view_rx)
}

#[tokio::test]
async fn model_switch_checks_persisted_media_before_mutating_state() {
    let (mut task, _) = event_task();
    let root = task.view.session.workspace_root.clone();
    task.config = Arc::new(serde_json::from_value(serde_json::json!({
        "providers": {"test": {"models": {
            "vision": {"name": "Vision", "modalities": {"input": ["text", "image"]}},
            "text": {"name": "Text", "modalities": {"input": ["text"]}},
            "other": {"name": "Other", "type": "anthropic", "modalities": {"input": ["text", "image"]}}
        }}}
    })).unwrap());
    let session = Arc::make_mut(&mut task.view.session);
    session.set_model("test/vision", None);
    session.turns = crate::provider::common::media_tool_session().turns;
    task.store.save_header(&task.view.session).await.unwrap();
    task.view.session = Arc::new(
        task.store
            .load_session(&task.view.session.id)
            .await
            .unwrap()
            .unwrap(),
    );
    task.view.persisted = true;
    let before = task.view.session.clone();
    let revision = task.view.revision;
    let error = task
        .configure(Some("test/text".into()), None, None)
        .await
        .unwrap_err();
    assert!(error.contains("does not support image input"));
    assert_eq!(task.view.revision, revision);
    assert_eq!(task.view.session.current_model, "test/vision");
    assert!(!task.state_path.exists());
    assert_eq!(
        task.store
            .load_session(&before.id)
            .await
            .unwrap()
            .unwrap()
            .current_model,
        "test/vision"
    );
    task.configure(Some("test/other".into()), None, None)
        .await
        .unwrap();
    assert_eq!(task.view.session.current_model, "test/other");
    assert_eq!(task.view.session.turns, before.turns);
    std::fs::remove_dir_all(root).unwrap();
}

struct BarrierTool(tokio::sync::Barrier);

#[async_trait::async_trait]
impl crate::tools::Tool for BarrierTool {
    fn name(&self) -> &str {
        "Barrier"
    }
    fn description(&self) -> &str {
        "Concurrency test"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"label": {"type": "string"}}})
    }
    async fn execute(&self, arguments: &str) -> Result<String, String> {
        self.0.wait().await;
        Ok(arguments.into())
    }
}

struct ParallelProvider(std::sync::atomic::AtomicBool);

#[async_trait::async_trait]
impl crate::provider::Provider for ParallelProvider {
    fn name(&self) -> &str {
        "parallel-test"
    }
    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        Default::default()
    }
    async fn complete(
        &self,
        _: &Session,
        _: &[crate::model::ToolDefinition],
        _: &crate::provider::CompletionOptions,
    ) -> Result<crate::provider::ProviderResponse, crate::provider::ProviderError> {
        let messages = if self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
            vec![Message::text("done")]
        } else {
            vec![Message::new(
                ["first", "second"]
                    .iter()
                    .map(|label| {
                        ContentPart::ToolCall(crate::model::ToolCall::new(
                            *label,
                            "Barrier",
                            serde_json::json!({"label": label}).to_string(),
                        ))
                    })
                    .collect(),
            )]
        };
        Ok(crate::provider::ProviderResponse {
            messages,
            usage: None,
            meta: Default::default(),
        })
    }
    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[crate::model::ToolDefinition],
        options: &crate::provider::CompletionOptions,
        _: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<crate::provider::ProviderResponse, crate::provider::ProviderError> {
        self.complete(session, tools, options).await
    }
}

#[tokio::test]
async fn parallel_tool_calls_run_concurrently_and_preserve_order() {
    let (mut task, _) = event_task();
    let root = task.view.session.workspace_root.clone();
    task.store.save_header(&task.view.session).await.unwrap();
    let mut tools = crate::tools::ToolRegistry::new();
    tools.register(Arc::new(BarrierTool(tokio::sync::Barrier::new(2))));
    let engine = AgentEngine {
        provider: Arc::new(ParallelProvider(std::sync::atomic::AtomicBool::new(false))),
        tools,
        options: crate::provider::CompletionOptions {
            enable_web_search: false,
            ..Default::default()
        },
        notices: Vec::new(),
        inputs: Default::default(),
    };
    let (_tx, mut rx) = mpsc::channel(1);
    let finished = tokio::time::timeout(Duration::from_secs(2), task.execute(engine, &mut rx))
        .await
        .unwrap()
        .unwrap();
    assert!(finished);
    let ids: Vec<_> = task
        .view
        .session
        .turns
        .iter()
        .flat_map(|turn| &turn.messages)
        .flat_map(|msg| &msg.parts)
        .filter_map(|part| match part {
            ContentPart::ToolResult(result) => Some(result.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["first", "second"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn late_citations_target_text_id_and_deduplicate_only_exact_spans() {
    let (mut task, mut view_rx) = event_task();
    task.event(AgentEvent::Text {
        text_id: "first".into(),
        delta: "Alpha ".into(),
    })
    .await;
    assert!(!view_rx.has_changed().unwrap());
    task.publish_throttled();
    assert!(view_rx.has_changed().unwrap());
    assert_eq!(
        view_rx.borrow_and_update().draft,
        vec![Arc::new(ContentPart::text("Alpha "))]
    );

    let search = WebSearchCall::completed("search", "sources", Vec::new());
    task.event(AgentEvent::WebSearchCall(search.clone())).await;
    task.event(AgentEvent::Text {
        text_id: "second".into(),
        delta: "Other text".into(),
    })
    .await;
    task.event(AgentEvent::Text {
        text_id: "first".into(),
        delta: "Beta".into(),
    })
    .await;

    let first = Annotation::UrlCitation {
        url: "https://example.com/source".into(),
        title: "Source".into(),
        start_index: 0,
        end_index: 5,
    };
    let second = Annotation::UrlCitation {
        url: "https://example.com/source".into(),
        title: "Source".into(),
        start_index: 6,
        end_index: 10,
    };
    for annotation in [&first, &first, &second, &second] {
        view_rx.borrow_and_update();
        task.event(AgentEvent::Citation {
            text_id: "first".into(),
            annotation: annotation.clone(),
        })
        .await;
        assert!(view_rx.has_changed().unwrap());
    }

    let published = view_rx.borrow_and_update().clone();
    assert_eq!(
        published.draft,
        vec![
            Arc::new(ContentPart::Text {
                text: "Alpha Beta".into(),
                annotations: vec![first, second],
            }),
            Arc::new(ContentPart::WebSearchCall(search)),
            Arc::new(ContentPart::text("Other text")),
        ]
    );
    assert!(matches!(published.state, RunState::Responding));
    assert!(published.session.turns.is_empty());
    assert!(!published.persisted);
    std::fs::remove_dir_all(&published.session.workspace_root).unwrap();
}

#[tokio::test]
async fn continued_text_after_search_moves_behind_work() {
    let (mut task, mut view_rx) = event_task();
    task.event(AgentEvent::Thought("plan".into())).await;
    task.event(AgentEvent::Text {
        text_id: "answer".into(),
        delta: "early ".into(),
    })
    .await;
    let search = WebSearchCall::completed("search", "query", Vec::new());
    task.event(AgentEvent::WebSearchCall(search.clone())).await;
    task.event(AgentEvent::Thought("review".into())).await;
    task.event(AgentEvent::Text {
        text_id: "answer".into(),
        delta: "later".into(),
    })
    .await;
    task.publish_throttled();
    let published = view_rx.borrow_and_update().clone();
    assert_eq!(
        published.draft,
        vec![
            Arc::new(ContentPart::reasoning("plan", None)),
            Arc::new(ContentPart::WebSearchCall(search)),
            Arc::new(ContentPart::reasoning("review", None)),
            Arc::new(ContentPart::text("early later")),
        ]
    );
    std::fs::remove_dir_all(&published.session.workspace_root).unwrap();
}

#[tokio::test]
async fn search_snapshots_update_in_place_and_publish_before_completion() {
    let (mut task, mut view_rx) = event_task();
    let pending = WebSearchCall {
        action: WebSearchAction::Search,
        id: "search".into(),
        query: String::new(),
        status: SearchStatus::Pending,
        sources: Vec::new(),
    };
    task.event(AgentEvent::WebSearchCall(pending.clone())).await;
    assert!(view_rx.has_changed().unwrap());
    let initial = view_rx.borrow_and_update().clone();
    assert_eq!(
        initial.draft,
        vec![Arc::new(ContentPart::WebSearchCall(pending.clone()))]
    );

    task.event(AgentEvent::Text {
        text_id: "answer".into(),
        delta: "Draft answer".into(),
    })
    .await;
    let other = WebSearchCall::completed("other-search", "other query", Vec::new());
    task.event(AgentEvent::WebSearchCall(other.clone())).await;

    let searching = WebSearchCall {
        query: "updated query".into(),
        status: SearchStatus::Searching,
        ..pending
    };
    let completed = WebSearchCall::completed(
        "search",
        "final query",
        vec![WebSearchSource::new(
            "https://example.com/source",
            "Source",
            Some("Result snippet".into()),
        )],
    );
    for snapshot in [searching, completed.clone(), completed] {
        let revision = view_rx.borrow_and_update().revision;
        task.event(AgentEvent::WebSearchCall(snapshot.clone()))
            .await;
        assert!(view_rx.has_changed().unwrap());
        let published = view_rx.borrow_and_update().clone();
        assert_eq!(published.revision, revision + 1);
        assert_eq!(
            published.draft,
            vec![
                Arc::new(ContentPart::WebSearchCall(snapshot)),
                Arc::new(ContentPart::text("Draft answer")),
                Arc::new(ContentPart::WebSearchCall(other.clone())),
            ]
        );
        assert!(matches!(published.state, RunState::Responding));
        assert!(published.session.turns.is_empty());
        assert!(!published.persisted);
    }
    assert_eq!(initial.draft.len(), 1);
    std::fs::remove_dir_all(&task.view.session.workspace_root).unwrap();
}

fn last_model_manager(dir: &Path) -> SessionManager {
    let store = Arc::new(JsonlSessionStore::new(dir).unwrap());
    let mut config = AppConfig::default();
    config.settings.default = "openai/default_model".into();
    config.providers.insert(
        "openai".into(),
        crate::config::ProviderConfig {
            api_key: "test".into(),
            ..Default::default()
        },
    );
    SessionManager::new(store, config, dir, dir.join("state.json"))
}

#[tokio::test]
async fn new_session_uses_last_selected_model() {
    let dir = std::env::temp_dir().join(format!("vva-last-model-{}", uuid::Uuid::new_v4()));
    let mgr = last_model_manager(&dir);
    let first = mgr
        .create_session("openai/first".into(), Some("high".into()), None, true)
        .await
        .unwrap();
    first
        .configure(Some("openai/second".into()), Some(Some("low".into())), None)
        .await
        .unwrap();

    let restarted = last_model_manager(&dir);
    let next = restarted
        .open_session(None, None, None, None)
        .await
        .unwrap();
    let snap = next.snapshot();
    assert_eq!(snap.session.current_model, "openai/second");
    assert_eq!(snap.session.current_variant.as_deref(), Some("low"));

    let explicit = restarted
        .open_session(None, Some("openai/explicit".into()), None, None)
        .await
        .unwrap();
    assert_eq!(explicit.snapshot().session.current_model, "openai/explicit");

    let alpha = restarted
        .create_session("openai/alpha".into(), None, None, true)
        .await
        .unwrap();
    restarted
        .create_session("openai/beta".into(), None, None, true)
        .await
        .unwrap();
    restarted.get_or_load(&alpha.id).await.unwrap();
    let after_switch = last_model_manager(&dir)
        .open_session(None, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        after_switch.snapshot().session.current_model,
        "openai/alpha"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

async fn seeded_session(dir: &Path) -> (SessionManager, Arc<ManagedSession>, String) {
    let store = Arc::new(JsonlSessionStore::new(dir).unwrap());
    let mut session = Session::with_model(
        dir,
        Some("Origin".into()),
        "openai/default_model",
        Some("high".into()),
        true,
    );
    session.append_turn(Turn::new(Role::System, vec![Message::text("sys")]));
    session.append_turn(Turn::user_turn(Message::text("first")));
    session.append_turn(Turn::assistant_turn(vec![Message::text("a1")]));
    session.append_turn(Turn::user_turn(Message::text("second")));
    session.append_turn(Turn::assistant_turn(vec![Message::text("a2")]));
    let id = session.id.clone();
    store.save_header(&session).await.unwrap();
    let mut config = AppConfig::default();
    config.settings.default = "openai/default_model".into();
    config.providers.insert(
        "openai".into(),
        crate::config::ProviderConfig {
            api_key: "test".into(),
            ..Default::default()
        },
    );
    let mgr = SessionManager::new(store, config, dir, dir.join("state.json"));
    let handle = mgr.get_or_load(&id).await.unwrap();
    (mgr, handle, id)
}

#[tokio::test]
async fn revert_truncates_from_user_turn_and_persists() {
    let dir = std::env::temp_dir().join(format!("vva-revert-{}", uuid::Uuid::new_v4()));
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
    let dir = std::env::temp_dir().join(format!("vva-fork-{}", uuid::Uuid::new_v4()));
    let (mgr, handle, id) = seeded_session(&dir).await;
    let forked = mgr
        .fork_session(&handle, 3, "Branched".into())
        .await
        .unwrap();
    let origin = handle.snapshot();
    let branch = forked.snapshot();
    assert_eq!(origin.session.turns.len(), 5);
    assert_eq!(origin.session.title.as_deref(), Some("Origin"));
    assert_eq!(branch.session.turns.len(), 3);
    assert_eq!(branch.session.title.as_deref(), Some("Branched"));
    assert_eq!(branch.session.current_model, "openai/default_model");
    assert_eq!(branch.session.current_variant.as_deref(), Some("high"));
    assert_eq!(branch.session.editor, origin.session.editor);
    assert_ne!(forked.id, id);
    let loaded = mgr.store().load_session(&forked.id).await.unwrap().unwrap();
    assert_eq!(loaded.turns.len(), 3);
    assert_eq!(loaded.title.as_deref(), Some("Branched"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn session_editor_frozen_at_create() {
    let dir = std::env::temp_dir().join(format!("vva-editor-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let mut config = AppConfig::default();
    config.settings.default = "p/plain".into();
    let mut models = std::collections::HashMap::new();
    models.insert(
        "plain".into(),
        crate::config::ModelConfig {
            name: "Plain".into(),
            ..Default::default()
        },
    );
    models.insert(
        "hashed".into(),
        crate::config::ModelConfig {
            name: "Hashed".into(),
            annotation: Some(crate::config::model::ModelAnnotation {
                editor: Some(crate::model::EditorKind::Hashline),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    models.insert(
        "patched".into(),
        crate::config::ModelConfig {
            name: "Patched".into(),
            annotation: Some(crate::config::model::ModelAnnotation {
                editor: Some(crate::model::EditorKind::Patch),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    config.providers.insert(
        "p".into(),
        crate::config::ProviderConfig {
            api_key: "test".into(),
            models,
            ..Default::default()
        },
    );
    let mgr = SessionManager::new(store, config, &dir, dir.join("state.json"));
    let handle = mgr
        .create_session("p/hashed".into(), None, None, true)
        .await
        .unwrap();
    assert_eq!(
        handle.snapshot().session.editor,
        crate::model::EditorKind::Hashline
    );
    handle
        .configure(Some("p/patched".into()), None, None)
        .await
        .unwrap();
    let snap = handle.snapshot();
    assert_eq!(snap.session.current_model, "p/patched");
    assert_eq!(snap.session.editor, crate::model::EditorKind::Hashline);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn session_editor_falls_back_to_settings_editor() {
    let dir = std::env::temp_dir().join(format!("vva-editor-default-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let mut config = AppConfig::default();
    config.settings.default = "p/plain".into();
    config.settings.editor = Some(crate::model::EditorKind::Patch);
    let mut models = std::collections::HashMap::new();
    models.insert(
        "plain".into(),
        crate::config::ModelConfig {
            name: "Plain".into(),
            ..Default::default()
        },
    );
    models.insert(
        "hashed".into(),
        crate::config::ModelConfig {
            name: "Hashed".into(),
            annotation: Some(crate::config::model::ModelAnnotation {
                editor: Some(crate::model::EditorKind::Hashline),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    config.providers.insert(
        "p".into(),
        crate::config::ProviderConfig {
            api_key: "test".into(),
            models,
            ..Default::default()
        },
    );
    let mgr = SessionManager::new(store, config, &dir, dir.join("state.json"));

    let plain = mgr
        .create_session("p/plain".into(), None, None, true)
        .await
        .unwrap();
    assert_eq!(
        plain.snapshot().session.editor,
        crate::model::EditorKind::Patch
    );

    // 模型级 annotation.editor 优先于 settings.editor
    let hashed = mgr
        .create_session("p/hashed".into(), None, None, true)
        .await
        .unwrap();
    assert_eq!(
        hashed.snapshot().session.editor,
        crate::model::EditorKind::Hashline
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn new_session_loads_project_agents_md() {
    let dir = std::env::temp_dir().join(format!("vva-agents-load-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("AGENTS.md"), "project instructions").unwrap();

    let (mut task, _rx) = event_task();
    let mut config = AppConfig::default();
    config.settings.default = "openai/test_model".into();
    config.providers.insert(
        "openai".into(),
        crate::config::ProviderConfig {
            api_key: "test".into(),
            ..Default::default()
        },
    );
    task.config = Arc::new(config);
    task.view.session = Arc::new(Session::with_model(
        &dir,
        None,
        "openai/test_model",
        None,
        false,
    ));
    task.store = Arc::new(JsonlSessionStore::new(&dir).unwrap());

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
