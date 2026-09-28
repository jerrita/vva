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
        external_web: Vec::new(),
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
        title_task: None,
        text_parts: HashMap::new(),
        ask_user_tx: mpsc::channel(1).0,
        ask_user_rx: mpsc::channel(1).1,
        pending_user_question: None,
        tasks: Arc::new(crate::tools::background::BackgroundTasks::new(Arc::new(
            |_| {},
        ))),
        task_completions: Vec::new(),
        subagent_completions: Vec::new(),
        pending_deliveries: Vec::new(),
        turn_tx: watch::channel(CompletedTurn::default()).0,
        turn_seq: 0,
        turn_start: 0,
        broker: Arc::new(StubBroker),
    };
    (task, view_rx)
}

/// 无编排能力的 broker 桩：SessionTask 单测路径不触发派生
struct StubBroker;

#[async_trait::async_trait]
impl crate::tools::agent::SessionBroker for StubBroker {
    async fn spawn_subagent(
        &self,
        _parent: &crate::tools::agent::SubagentContext,
        _def: &crate::agents::AgentDef,
        _prompt: &str,
        _description: &str,
        _background: bool,
    ) -> Result<crate::tools::agent::SubagentLaunch, String> {
        Err("stub broker".into())
    }
    async fn deliver(&self, _session_id: &str, _text: &str) -> Result<String, String> {
        Err("stub broker".into())
    }
    async fn wait_turn(
        &self,
        _session_id: &str,
        _seen_seq: u64,
        _timeout: Option<std::time::Duration>,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        Err("stub broker".into())
    }
    async fn session_report(
        &self,
        _session_id: &str,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        Err("stub broker".into())
    }
    async fn abort_session(&self, _session_id: &str) -> Result<(), String> {
        Err("stub broker".into())
    }
    async fn abort_children(&self, _parent_id: &str) {}
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

/// 等待后台任务达到期望终态（真实子进程，轮询 snapshot）
async fn wait_for_task(
    task: &SessionTask,
    id: &str,
    want: crate::tools::background::BackgroundStatus,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status = task.tasks.snapshot(id).expect("task present").status;
        if status == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background task stuck at {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn task_completion_injects_persisted_user_turn() {
    let (mut task, _view) = event_task();
    let root = task.view.session.workspace_root.clone();
    task.store.save_header(&task.view.session).await.unwrap();
    let id = task
        .tasks
        .spawn(
            "watch build",
            if cfg!(windows) {
                "Write-Output done"
            } else {
                "echo done"
            },
            &root,
        )
        .unwrap();
    wait_for_task(
        &task,
        &id,
        crate::tools::background::BackgroundStatus::Exited { code: Some(0) },
    )
    .await;

    // 完成状态关联任务身份
    task.queue_task_completion(id.clone()).await;
    assert!(
        task.view
            .notices
            .last()
            .is_some_and(|notice| notice.text.contains("exited with code 0")),
        "{:?}",
        task.view.notices
    );

    task.drain_task_completions().await.unwrap();
    assert!(task.task_completions.is_empty());
    let text = task.view.session.turns.last().unwrap().text_content();
    // 注入轮次为 background-task reminder：首行为状态，机器注入可识别
    assert!(
        text.contains("<system-reminder type=\"background-task\">"),
        "{text}"
    );
    assert!(text.ends_with("</system-reminder>"), "{text}");
    assert!(
        text.contains("Background task 1 exited with code 0"),
        "{text}"
    );
    assert!(text.contains("done"), "{text}");
    assert!(text.contains("TaskOutput"), "{text}");
    let loaded = task
        .store
        .load_session(&task.view.session.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        loaded
            .turns
            .last()
            .unwrap()
            .text_content()
            .contains("background-task")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn drain_skips_vanished_task_without_error() {
    let (mut task, _view) = event_task();
    task.task_completions.push("404".into());

    task.drain_task_completions().await.unwrap();

    assert!(task.view.session.turns.is_empty());
    assert!(task.task_completions.is_empty());
    let _ = std::fs::remove_dir_all(&task.view.session.workspace_root);
}

/// 直接返回文本的供应商桩：execute 注入通知后应立即收尾
struct PlainTextProvider;

#[async_trait::async_trait]
impl crate::provider::Provider for PlainTextProvider {
    fn name(&self) -> &str {
        "plain-test"
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
        Ok(crate::provider::ProviderResponse {
            messages: vec![Message::text("done")],
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
async fn execute_drains_completions_before_next_model_call() {
    let (mut task, _view) = event_task();
    let root = task.view.session.workspace_root.clone();
    task.store.save_header(&task.view.session).await.unwrap();
    let id = task
        .tasks
        .spawn(
            "quick",
            if cfg!(windows) { "exit 0" } else { "true" },
            &root,
        )
        .unwrap();
    wait_for_task(
        &task,
        &id,
        crate::tools::background::BackgroundStatus::Exited { code: Some(0) },
    )
    .await;
    // 空闲路径也发布任务完成状态
    task.queue_task_completion(id).await;
    assert_eq!(task.view.notices.len(), 1);

    let engine = AgentEngine {
        provider: Arc::new(PlainTextProvider),
        tools: crate::tools::ToolRegistry::new(),
        options: crate::provider::CompletionOptions::default(),
        notices: Vec::new(),
        external_web: Vec::new(),
        inputs: Default::default(),
    };
    let (_tx, mut rx) = mpsc::channel(1);
    let finished = task.execute(engine, &mut rx).await.unwrap();
    assert!(finished);

    let turns = &task.view.session.turns;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, Role::User);
    assert!(turns[0].text_content().contains("background-task"));
    assert!(crate::system::parse_agents_reminder(&turns[0].text_content()).is_some());
    assert_eq!(turns[1].role, Role::Assistant);
    assert_eq!(turns[1].text_content(), "done");
    let _ = std::fs::remove_dir_all(root);
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
        external_web: Vec::new(),
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

fn last_model_manager(dir: &Path) -> Arc<SessionManager> {
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
    Arc::new(SessionManager::new(
        store,
        config,
        dir,
        dir.join("state.json"),
    ))
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

async fn seeded_session(dir: &Path) -> (Arc<SessionManager>, Arc<ManagedSession>, String) {
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
    let mgr = Arc::new(SessionManager::new(
        store,
        config,
        dir,
        dir.join("state.json"),
    ));
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
    let mgr = Arc::new(SessionManager::new(
        store,
        config,
        &dir,
        dir.join("state.json"),
    ));
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
    let mgr = Arc::new(SessionManager::new(
        store,
        config,
        &dir,
        dir.join("state.json"),
    ));

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

// ---- Subagent 调度与跨会话投递 ----

/// 返回固定报告的 broker 桩：驱动 drain_subagent_completions 与 wait 路径
struct ReportingBroker(crate::tools::agent::SubagentReport);

#[async_trait::async_trait]
impl crate::tools::agent::SessionBroker for ReportingBroker {
    async fn spawn_subagent(
        &self,
        _parent: &crate::tools::agent::SubagentContext,
        _def: &crate::agents::AgentDef,
        _prompt: &str,
        _description: &str,
        _background: bool,
    ) -> Result<crate::tools::agent::SubagentLaunch, String> {
        Err("stub broker".into())
    }
    async fn deliver(&self, _session_id: &str, _text: &str) -> Result<String, String> {
        Err("stub broker".into())
    }
    async fn wait_turn(
        &self,
        _session_id: &str,
        _seen_seq: u64,
        _timeout: Option<std::time::Duration>,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        Err("stub broker".into())
    }
    async fn session_report(
        &self,
        _session_id: &str,
    ) -> Result<crate::tools::agent::SubagentReport, String> {
        Ok(crate::tools::agent::SubagentReport {
            session_id: self.0.session_id.clone(),
            agent: self.0.agent.clone(),
            title: self.0.title.clone(),
            turn_seq: self.0.turn_seq,
            error: None,
            text: self.0.text.clone(),
        })
    }
    async fn abort_session(&self, _session_id: &str) -> Result<(), String> {
        Err("stub broker".into())
    }
    async fn abort_children(&self, _parent_id: &str) {}
}

/// 发起一次 ToolCall 的供应商桩：工具阶段挂起供 Stop 中止
struct ToolCallProvider;

#[async_trait::async_trait]
impl crate::provider::Provider for ToolCallProvider {
    fn name(&self) -> &str {
        "tool-call-test"
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
        Ok(crate::provider::ProviderResponse {
            messages: vec![Message::new(vec![ContentPart::ToolCall(
                crate::model::ToolCall::new("call_hang", "Hang", "{}"),
            )])],
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

/// 永不完成的工具：模拟长任务，验证 Stop 中止时的结果合成
struct HangTool;

#[async_trait::async_trait]
impl crate::tools::Tool for HangTool {
    fn name(&self) -> &str {
        "Hang"
    }
    fn description(&self) -> &str {
        "test tool that never finishes"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn execute(&self, _arguments: &str) -> Result<String, String> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(String::new())
    }
}

#[tokio::test]
async fn abort_during_tools_synthesizes_paired_error_results() {
    let (mut task, _view) = event_task();
    let root = task.view.session.workspace_root.clone();
    task.store.save_header(&task.view.session).await.unwrap();
    task.view.persisted = true;
    let mut tools = crate::tools::ToolRegistry::new();
    tools.register(Arc::new(HangTool));
    let engine = AgentEngine {
        provider: Arc::new(ToolCallProvider),
        tools,
        options: crate::provider::CompletionOptions::default(),
        notices: Vec::new(),
        external_web: Vec::new(),
        inputs: Default::default(),
    };
    let (tx, mut rx) = mpsc::channel(4);
    let stopper = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (reply, _) = oneshot::channel();
        let _ = stopper
            .send(Command::Stop {
                delete: false,
                reply,
            })
            .await;
    });
    let finished = task.execute(engine, &mut rx).await.unwrap();
    assert!(!finished);

    // assistant 轮携带 ToolCall，随后必须配对合成 error ToolResult（无悬空 tool_call）
    let turns = &task.view.session.turns;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, Role::Assistant);
    assert_eq!(turns[1].role, Role::Tool);
    match turns[1].messages[0].parts[0] {
        ContentPart::ToolResult(ref result) => {
            assert_eq!(result.tool_call_id, "call_hang");
            assert!(result.is_error);
            assert!(result.content.contains("aborted"), "{}", result.content);
        }
        ref other => panic!("expected ToolResult, got {other:?}"),
    }
    let loaded = task
        .store
        .load_session(&task.view.session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.turns.len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn deliver_while_running_is_queued_and_acked() {
    let (mut task, _view) = event_task();
    let (tx, mut rx) = mpsc::channel(4);
    let (_events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(1);
    let (reply, reply_rx) = oneshot::channel();
    tokio::spawn({
        let tx = tx.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx
                .send(Command::Deliver {
                    delivery: PendingDelivery {
                        text: "hello".into(),
                        completion: oneshot::channel().0,
                    },
                    reply,
                })
                .await;
        }
    });
    let outcome = task
        .drive(
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                7
            },
            &mut rx,
            &mut events_rx,
        )
        .await;
    assert_eq!(outcome, Some(7));
    reply_rx.await.unwrap().unwrap();
    assert_eq!(task.pending_deliveries.len(), 1);
    assert_eq!(task.pending_deliveries[0].text, "hello");
    let _ = std::fs::remove_dir_all(&task.view.session.workspace_root);
}

#[tokio::test]
async fn pending_deliveries_run_as_turns_and_bump_seq() {
    let (mut task, _view) = event_task();
    let seq_rx = task.turn_tx.subscribe();
    let root = task.view.session.workspace_root.clone();
    // 不可达端点：投递轮次快速失败（不出本机），验证冲刷机制而非补全结果
    let mut config = AppConfig::default();
    let mut models = std::collections::HashMap::new();
    models.insert(
        "m".into(),
        crate::config::ModelConfig {
            name: "M".into(),
            ..Default::default()
        },
    );
    config.providers.insert(
        "p".into(),
        crate::config::ProviderConfig {
            api_key: "k".into(),
            base_url: "http://127.0.0.1:1".into(),
            models,
            ..Default::default()
        },
    );
    task.config = Arc::new(config);
    Arc::make_mut(&mut task.view.session).set_model("p/m".to_string(), None);
    task.pending_deliveries.push(PendingDelivery {
        text: "queued message".into(),
        completion: oneshot::channel().0,
    });
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
    assert_eq!(task.turn_seq, 1);
    assert_eq!(seq_rx.borrow().seq, 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn drain_subagent_completions_injects_final_message_reminder() {
    let (mut task, _view) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    task.broker = Arc::new(ReportingBroker(crate::tools::agent::SubagentReport {
        session_id: "sess_kid1".into(),
        agent: "explore".into(),
        title: "[explore] probe".into(),
        turn_seq: 1,
        error: None,
        text: "found 3 matches".into(),
    }));
    task.subagent_completions.push("sess_kid1".into());
    task.drain_subagent_completions().await.unwrap();

    let text = task.view.session.turns.last().unwrap().text_content();
    assert!(
        text.contains("<system-reminder type=\"background-task\">"),
        "{text}"
    );
    assert!(
        text.contains("Background agent sess_kid1 (explore) finished."),
        "{text}"
    );
    assert!(text.contains("found 3 matches"), "{text}");
    assert!(task.subagent_completions.is_empty());
    // 报告失败（会话消失）：跳过不报错
    task.broker = Arc::new(StubBroker);
    task.subagent_completions.push("sess_gone".into());
    task.drain_subagent_completions().await.unwrap();
    assert_eq!(task.view.session.turns.len(), 1);
    let _ = std::fs::remove_dir_all(&task.view.session.workspace_root);
}

/// 子代理派生链路（不可达端点让子会话轮次快速失败，不出本机）：
/// 元数据落盘、注册表可见、wait_turn 返回空文本报告
#[tokio::test]
async fn spawn_subagent_creates_child_session_and_wait_reports() {
    use crate::tools::agent::SessionBroker as _;
    let dir = std::env::temp_dir().join(format!("vva-subagent-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let mut config = AppConfig::default();
    config.settings.default = "p/m".into();
    let mut models = std::collections::HashMap::new();
    models.insert(
        "m".into(),
        crate::config::ModelConfig {
            name: "M".into(),
            ..Default::default()
        },
    );
    config.providers.insert(
        "p".into(),
        crate::config::ProviderConfig {
            api_key: "k".into(),
            base_url: "http://127.0.0.1:1".into(),
            models,
            ..Default::default()
        },
    );
    let manager = Arc::new(SessionManager::new(
        store,
        config,
        &dir,
        dir.join("state.json"),
    ));
    let parent = manager
        .create_session("p/m".into(), None, None, true)
        .await
        .unwrap();
    let broker = super::manager::SessionBrokerHandle::new(manager.clone());
    let def = crate::agents::AgentDef {
        name: "explore".into(),
        description: "test".into(),
        model: None,
        tools: vec!["Read".into()],
        system_prompt: None,
    };
    let launch = broker
        .spawn_subagent(
            &crate::tools::agent::SubagentContext::from_session(&parent.snapshot().session),
            &def,
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
    assert_eq!(loaded.agent.as_deref(), Some("explore"));
    assert_eq!(loaded.parent.as_deref(), Some(parent.id.as_str()));
    assert_eq!(loaded.depth, 1);
    assert_eq!(loaded.current_model, "p/m");

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
    use crate::tools::agent::SessionBroker as _;
    let dir = std::env::temp_dir().join(format!("vva-subagent-depth-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let mut config = AppConfig::default();
    config.settings.default = "p/m".into();
    let mut models = std::collections::HashMap::new();
    models.insert(
        "m".into(),
        crate::config::ModelConfig {
            name: "M".into(),
            ..Default::default()
        },
    );
    config.providers.insert(
        "p".into(),
        crate::config::ProviderConfig {
            api_key: "k".into(),
            base_url: "http://127.0.0.1:1".into(),
            models,
            ..Default::default()
        },
    );
    let manager = Arc::new(SessionManager::new(
        store,
        config,
        &dir,
        dir.join("state.json"),
    ));
    let parent = manager
        .create_session("p/m".into(), None, None, true)
        .await
        .unwrap();
    let broker = super::manager::SessionBrokerHandle::new(manager.clone());
    let mut context =
        crate::tools::agent::SubagentContext::from_session(&parent.snapshot().session);
    context.depth = crate::agents::MAX_SUBAGENT_DEPTH;
    let def = crate::agents::AgentDef {
        name: "general".into(),
        description: "test".into(),
        model: None,
        tools: vec!["Read".into()],
        system_prompt: None,
    };
    let error = broker
        .spawn_subagent(&context, &def, "prompt", "desc", false)
        .await
        .unwrap_err();
    assert!(error.contains("nesting depth"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 投递回复先于 WaitSession 完成时，等待仍返回对应回执。
#[tokio::test]
async fn wait_turn_returns_delivered_reply_after_fast_completion() {
    use crate::tools::agent::SessionBroker as _;
    let dir = std::env::temp_dir().join(format!("vva-wait-receipt-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let mut config = AppConfig::default();
    config.settings.default = "p/m".into();
    let mut models = std::collections::HashMap::new();
    models.insert(
        "m".into(),
        crate::config::ModelConfig {
            name: "M".into(),
            ..Default::default()
        },
    );
    config.providers.insert(
        "p".into(),
        crate::config::ProviderConfig {
            api_key: "k".into(),
            base_url: "http://127.0.0.1:1".into(),
            models,
            ..Default::default()
        },
    );
    let manager = Arc::new(SessionManager::new(
        store,
        config,
        &dir,
        dir.join("state.json"),
    ));
    let parent = manager
        .create_session("p/m".into(), None, None, true)
        .await
        .unwrap();
    let broker = super::manager::SessionBrokerHandle::new(manager.clone());
    let def = crate::agents::AgentDef {
        name: "general".into(),
        description: "test".into(),
        model: None,
        tools: vec!["Read".into()],
        system_prompt: None,
    };
    let launch = broker
        .spawn_subagent(
            &crate::tools::agent::SubagentContext::from_session(&parent.snapshot().session),
            &def,
            "find things",
            "probe",
            false,
        )
        .await
        .unwrap();
    // 首轮（派生提示词）在不可达端点上快速失败结束
    let first = broker
        .wait_turn(
            &launch.session_id,
            launch.turn_seq,
            Some(Duration::from_secs(15)),
        )
        .await
        .unwrap();

    // 子会话空闲：投递返回独立回执并触发回复轮。
    let delivered = broker
        .deliver(&launch.session_id, "reply ok")
        .await
        .unwrap();
    assert!(delivered.starts_with("Delivered;"), "{delivered}");
    // 等待回复轮结束，保留回执供后续 WaitSession 消费。
    let handle = manager.get_or_load(&launch.session_id).await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while handle.turn_seq() <= first.turn_seq {
        assert!(
            std::time::Instant::now() < deadline,
            "reply turn did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // 复现竞态时序：WaitSession 在回复轮完成后才计算调用方计数
    let stale_seen = broker
        .session_report(&launch.session_id)
        .await
        .unwrap()
        .turn_seq;
    assert_eq!(stale_seen, first.turn_seq + 1);

    // 回执保存本次回复轮报告，立即返回。
    let reply = broker
        .wait_turn(
            &launch.session_id,
            stale_seen,
            Some(Duration::from_secs(15)),
        )
        .await
        .unwrap();
    assert_eq!(reply.turn_seq, stale_seen);

    // 回执消费后，下一次等待订阅新的完成轮次。
    let started = std::time::Instant::now();
    let again = broker
        .wait_turn(&launch.session_id, stale_seen, Some(Duration::from_secs(1)))
        .await;
    assert!(again.is_err());
    assert!(started.elapsed() >= Duration::from_millis(900));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 删除会话级联清理子会话：活跃与冷存的派生链递归删盘，无关会话保留
#[tokio::test]
async fn delete_session_cascades_to_child_sessions() {
    let dir = std::env::temp_dir().join(format!("vva-delete-cascade-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let manager = Arc::new(SessionManager::new(
        store.clone(),
        AppConfig::default(),
        &dir,
        dir.join("state.json"),
    ));

    let parent = Session::with_model(&dir, Some("parent".into()), "p/m", None, true);
    let mut live_child = Session::with_model(&dir, Some("child".into()), "p/m", None, true);
    live_child.parent = Some(parent.id.clone());
    let mut cold_child = Session::with_model(&dir, Some("cold".into()), "p/m", None, true);
    cold_child.parent = Some(parent.id.clone());
    let mut grandchild = Session::with_model(&dir, Some("grand".into()), "p/m", None, true);
    grandchild.parent = Some(live_child.id.clone());
    let orphan = Session::with_model(&dir, Some("orphan".into()), "p/m", None, true);
    for session in [&parent, &live_child, &cold_child, &grandchild, &orphan] {
        store.save_header(session).await.unwrap();
    }

    let parent_handle = manager.get_or_load(&parent.id).await.unwrap();
    let _live_handle = manager.get_or_load(&live_child.id).await.unwrap();

    manager.delete_session(&parent_handle).await.unwrap();

    assert!(
        manager
            .store()
            .load_session(&parent.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .store()
            .load_session(&live_child.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .store()
            .load_session(&cold_child.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .store()
            .load_session(&grandchild.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .store()
            .load_session(&orphan.id)
            .await
            .unwrap()
            .is_some()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn failed_or_aborted_turn_has_no_old_answer_and_cancels_pending_deliveries() {
    for outcome in [Ok(false), Err("provider failure".to_string())] {
        let (mut task, _) = event_task();
        Arc::make_mut(&mut task.view.session)
            .append_turn(Turn::assistant_turn(vec![Message::text("earlier answer")]));
        task.turn_start = task.view.session.turns.len();
        let (completion, receipt) = oneshot::channel();
        task.pending_deliveries.push(PendingDelivery {
            text: "queued request".into(),
            completion,
        });
        assert!(!task.finish_turn(outcome).await);
        assert!(receipt.await.unwrap().is_err());
        assert!(task.pending_deliveries.is_empty());
        assert!(task.turn_tx.borrow().text.is_empty());
        assert!(task.turn_tx.borrow().error.is_some());
        let (_, mut commands) = mpsc::channel(1);
        assert!(!task.run_pending_deliveries(&mut commands).await);
        assert_eq!(task.view.session.turns.len(), 1);
        assert_eq!(task.turn_seq, 1);
        let _ = std::fs::remove_dir_all(&task.view.session.workspace_root);
    }
}

#[tokio::test]
async fn notice_lifecycle_uses_identity_and_current_capabilities() {
    let (mut task, view) = event_task();
    task.event(AgentEvent::Notice("First feedback".into()))
        .await;
    let first = task.view.notices[0].id.clone();
    task.event(AgentEvent::Notice("Second feedback".into()))
        .await;
    let second = task.view.notices[1].id.clone();
    task.dismiss_notice(&first).unwrap();
    task.dismiss_notice(&first).unwrap();
    assert_eq!(view.borrow().notices[0].id, second);

    task.event(AgentEvent::Capabilities(vec!["Feature unavailable".into()]))
        .await;
    let capability = task.view.notices.last().unwrap().id.clone();
    task.event(AgentEvent::Capabilities(vec!["Feature unavailable".into()]))
        .await;
    assert_eq!(task.view.notices.last().unwrap().id, capability);
    task.event(AgentEvent::Capabilities(vec![
        "Different limitation".into(),
    ]))
    .await;
    assert_eq!(task.view.notices.len(), 2);
    assert_eq!(
        task.view.notices.last().unwrap().text,
        "Different limitation"
    );
    task.event(AgentEvent::Capabilities(Vec::new())).await;
    assert_eq!(task.view.notices.len(), 1);
    assert_eq!(task.view.notices[0].id, second);
}

#[tokio::test]
async fn turn_failure_keeps_its_anchor_when_another_turn_starts() {
    let (mut task, _) = event_task();
    let request = Turn::user_turn(Message::text("First request"));
    let id = request.id.clone();
    Arc::make_mut(&mut task.view.session).append_turn(request);
    task.view
        .draft
        .push(Arc::new(ContentPart::text("Partial response")));
    task.event(AgentEvent::Error("Request failed".into())).await;
    Arc::make_mut(&mut task.view.session)
        .append_turn(Turn::user_turn(Message::text("Next request")));
    assert!(task.view.draft.is_empty());
    assert!(
        matches!(&task.view.notices[0].kind, NoticeKind::Turn { turn_id, aborted: false } if turn_id == &id)
    );
}
