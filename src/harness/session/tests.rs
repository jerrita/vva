use super::task::*;
use super::*;
use crate::config::{ModelConfig, ProviderConfig, model::ModelAnnotation};
use crate::harness::testing::StubBroker;
use crate::model::{
    Annotation, EditorKind, SearchStatus, ToolCall, WebSearchAction, WebSearchCall, WebSearchSource,
};
use crate::provider::{CompletionOptions, ProviderResponse};
use crate::storage::jsonl::JsonlSessionStore;
use crate::tools::agent::{SessionBroker as _, SubagentContext, SubagentReport};
use crate::tools::background::BackgroundStatus;

// ---- 共享夹具 ----

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vva-{tag}-{}", uuid::Uuid::new_v4()))
}

fn task_with(
    session: Session,
    store: Arc<dyn SessionStore>,
) -> (SessionTask, watch::Receiver<Arc<SessionView>>) {
    let state_path = session.header.workspace_root.join("state.json");
    let (commands, _rx) = mpsc::channel(1);
    let (task, view, _) = SessionTask::new(
        session,
        false,
        TaskDeps {
            store,
            config: Arc::new(AppConfig::default()),
            state_path,
            broker: Arc::new(StubBroker(None)),
            tool_factory: None,
            preamble: None,
        },
        commands.downgrade(),
    );
    (task, view)
}

fn event_task() -> (SessionTask, watch::Receiver<Arc<SessionView>>) {
    let workspace = tmp("events");
    let store = Arc::new(JsonlSessionStore::new(&workspace).unwrap());
    task_with(Session::new(workspace, None), store)
}

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

/// 按调用次序产出响应的供应商桩：首次调用取 `script(false)`，之后取 `script(true)`
struct ScriptedProvider {
    script: fn(bool) -> Vec<Message>,
    called: std::sync::atomic::AtomicBool,
}

impl ScriptedProvider {
    fn new(script: fn(bool) -> Vec<Message>) -> Arc<Self> {
        Arc::new(Self {
            script,
            called: Default::default(),
        })
    }
}

#[async_trait::async_trait]
impl crate::provider::Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted-test"
    }
    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        Default::default()
    }
    async fn complete(
        &self,
        _: &Session,
        _: &[crate::model::ToolDefinition],
        _: &CompletionOptions,
    ) -> Result<ProviderResponse, crate::provider::ProviderError> {
        let repeat = self.called.swap(true, std::sync::atomic::Ordering::SeqCst);
        Ok(ProviderResponse {
            messages: (self.script)(repeat),
            usage: None,
            meta: Default::default(),
        })
    }
    async fn complete_stream(
        &self,
        session: &Session,
        tools: &[crate::model::ToolDefinition],
        options: &CompletionOptions,
        _: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<ProviderResponse, crate::provider::ProviderError> {
        self.complete(session, tools, options).await
    }
}

fn engine(provider: Arc<ScriptedProvider>, tools: crate::tools::ToolRegistry) -> AgentEngine {
    AgentEngine {
        provider,
        tools,
        options: CompletionOptions::default(),
        notices: Vec::new(),
        external_web: Vec::new(),
        inputs: Default::default(),
    }
}

fn done_provider() -> Arc<ScriptedProvider> {
    ScriptedProvider::new(|_| vec![Message::text("done")])
}

/// 测试工具：可选阻塞语义，参数原样回显
struct TestTool {
    name: &'static str,
    /// 等待栅栏 (并发验证) 或永不完成 (中止验证)
    blocking: TestBlock,
}

enum TestBlock {
    Barrier(tokio::sync::Barrier),
    Forever,
}

#[async_trait::async_trait]
impl crate::tools::Tool for TestTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"label": {"type": "string"}}})
    }
    async fn execute(&self, arguments: &str) -> Result<String, String> {
        match &self.blocking {
            TestBlock::Barrier(barrier) => {
                barrier.wait().await;
                Ok(arguments.into())
            }
            TestBlock::Forever => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(String::new())
            }
        }
    }
}

fn registry_with(tool: TestTool) -> crate::tools::ToolRegistry {
    let mut tools = crate::tools::ToolRegistry::new();
    tools.register(Arc::new(tool));
    tools
}

/// 等待后台任务达到期望终态（真实子进程，轮询 snapshot）
async fn wait_for_task(task: &SessionTask, id: &str, want: BackgroundStatus) {
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

/// 已落盘头部的空闲任务与一个已退出 (code 0) 的后台任务 id
async fn task_with_finished_background(command: &str) -> (SessionTask, String) {
    let (task, _view) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    let root = task.view.session.header.workspace_root.clone();
    let id = task.tasks.spawn("bg", command, &root).unwrap();
    wait_for_task(&task, &id, BackgroundStatus::Exited { code: Some(0) }).await;
    (task, id)
}

fn cleanup(task: &SessionTask) {
    let _ = std::fs::remove_dir_all(&task.view.session.header.workspace_root);
}

// ---- 配置与模型切换 ----

#[tokio::test]
async fn model_switch_checks_persisted_media_before_mutating_state() {
    let (mut task, _) = event_task();
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
    assert!(!task.state_path.exists());
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
    task.queue_task_completion(id).await;
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
    for expected in ["Background task 1 exited with code 0", "done", "TaskOutput"] {
        assert!(text.contains(expected), "{text}");
    }
    let loaded = task
        .store
        .load_session(&task.view.session.header.id)
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
    cleanup(&task);
}

#[tokio::test]
async fn drain_skips_vanished_task_without_error() {
    let (mut task, _view) = event_task();
    task.task_completions.push("404".into());

    task.drain_task_completions().await.unwrap();

    assert!(task.view.session.turns.is_empty());
    assert!(task.task_completions.is_empty());
    cleanup(&task);
}

#[tokio::test]
async fn execute_drains_completions_before_next_model_call() {
    let (mut task, id) =
        task_with_finished_background(if cfg!(windows) { "exit 0" } else { "true" }).await;
    // 空闲路径也发布任务完成状态
    task.queue_task_completion(id).await;
    assert_eq!(task.view.notices.len(), 1);

    let (_tx, mut rx) = mpsc::channel(1);
    let finished = task
        .execute(engine(done_provider(), Default::default()), &mut rx)
        .await
        .unwrap();
    assert_eq!(finished, TurnOutcome::Completed);

    let turns = &task.view.session.turns;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, Role::User);
    assert!(turns[0].text_content().contains("background-task"));
    assert!(crate::system::parse_agents_reminder(&turns[0].text_content()).is_some());
    assert_eq!(turns[1].role, Role::Assistant);
    assert_eq!(turns[1].text_content(), "done");
    cleanup(&task);
}

#[tokio::test]
async fn drain_subagent_completions_injects_final_message_reminder() {
    let (mut task, _view) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    task.broker = Arc::new(StubBroker(Some(SubagentReport {
        session_id: "sess_kid1".into(),
        agent: "explore".into(),
        title: "[explore] probe".into(),
        turn_seq: 1,
        error: None,
        text: "found 3 matches".into(),
    })));
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
    task.broker = Arc::new(StubBroker(None));
    task.subagent_completions.push("sess_gone".into());
    task.drain_subagent_completions().await.unwrap();
    assert_eq!(task.view.session.turns.len(), 1);
    cleanup(&task);
}

// ---- 工具执行 ----

#[tokio::test]
async fn parallel_tool_calls_run_concurrently_and_preserve_order() {
    let (mut task, _) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    let tools = registry_with(TestTool {
        name: "Barrier",
        blocking: TestBlock::Barrier(tokio::sync::Barrier::new(2)),
    });
    let provider = ScriptedProvider::new(|repeat| {
        if repeat {
            return vec![Message::text("done")];
        }
        let calls = ["first", "second"].map(|label| {
            ContentPart::ToolCall(ToolCall::new(
                label,
                "Barrier",
                serde_json::json!({"label": label}).to_string(),
            ))
        });
        vec![Message::new(calls.into())]
    });
    let (_tx, mut rx) = mpsc::channel(1);
    let finished = tokio::time::timeout(
        Duration::from_secs(2),
        task.execute(engine(provider, tools), &mut rx),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(finished, TurnOutcome::Completed);
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
    cleanup(&task);
}

#[tokio::test]
async fn abort_during_tools_synthesizes_paired_error_results() {
    let (mut task, _view) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    task.view.persisted = true;
    let tools = registry_with(TestTool {
        name: "Hang",
        blocking: TestBlock::Forever,
    });
    let provider = ScriptedProvider::new(|_| {
        vec![Message::new(vec![ContentPart::ToolCall(ToolCall::new(
            "call_hang",
            "Hang",
            "{}",
        ))])]
    });
    let (tx, mut rx) = mpsc::channel(4);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (reply, _) = oneshot::channel();
        let _ = tx
            .send(Command::Stop {
                delete: false,
                reply,
            })
            .await;
    });
    let finished = task
        .execute(engine(provider, tools), &mut rx)
        .await
        .unwrap();
    assert_eq!(finished, TurnOutcome::Aborted);

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
        .load_session(&task.view.session.header.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.turns.len(), 2);
    cleanup(&task);
}

// ---- 草稿事件流 ----

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
    for (text_id, delta) in [("second", "Other text"), ("first", "Beta")] {
        task.event(AgentEvent::Text {
            text_id: text_id.into(),
            delta: delta.into(),
        })
        .await;
    }

    let citation = |start_index, end_index| Annotation::UrlCitation {
        url: "https://example.com/source".into(),
        title: "Source".into(),
        start_index,
        end_index,
    };
    let (first, second) = (citation(0, 5), citation(6, 10));
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
    cleanup(&task);
}

#[tokio::test]
async fn continued_text_after_search_moves_behind_work() {
    let (mut task, mut view_rx) = event_task();
    let search = WebSearchCall::completed("search", "query", Vec::new());
    let events = [
        AgentEvent::Thought("plan".into()),
        AgentEvent::Text {
            text_id: "answer".into(),
            delta: "early ".into(),
        },
        AgentEvent::WebSearchCall(search.clone()),
        AgentEvent::Thought("review".into()),
        AgentEvent::Text {
            text_id: "answer".into(),
            delta: "later".into(),
        },
    ];
    for event in events {
        task.event(event).await;
    }
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
    cleanup(&task);
}

#[tokio::test]
async fn search_snapshots_update_in_place_and_publish_before_completion() {
    let (mut task, mut view_rx) = event_task();
    let pending = WebSearchCall {
        action: Some(WebSearchAction::Search),
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
    cleanup(&task);
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
    task.dismiss_notice(&first);
    task.dismiss_notice(&first);
    assert_eq!(view.borrow().notices[0].id, second);

    let capabilities =
        |texts: &[&str]| AgentEvent::Capabilities(texts.iter().map(|t| t.to_string()).collect());
    task.event(capabilities(&["Feature unavailable"])).await;
    let capability = task.view.notices.last().unwrap().id.clone();
    task.event(capabilities(&["Feature unavailable"])).await;
    assert_eq!(task.view.notices.last().unwrap().id, capability);
    task.event(capabilities(&["Different limitation"])).await;
    assert_eq!(task.view.notices.len(), 2);
    assert_eq!(
        task.view.notices.last().unwrap().text,
        "Different limitation"
    );
    task.event(capabilities(&[])).await;
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

#[tokio::test]
async fn team_toggle_configures_and_persists_independently() {
    let dir = tmp("team-toggle");
    let mgr = manager_for(&dir, openai_config());
    let handle = mgr
        .create_session(
            "openai/first".into(),
            None,
            None,
            crate::model::SessionToggles {
                web_search: false,
                team: true,
            },
        )
        .await
        .unwrap();
    handle
        .configure(
            None,
            None,
            crate::model::ToggleOverrides {
                team: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let header = handle.snapshot().session.header.clone();
    assert!(!header.team);
    assert!(!header.web_search, "team 覆写不应改动 web 开关");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_header_defaults_team_enabled() {
    let header: crate::model::SessionHeader = serde_json::from_value(serde_json::json!({
        "id": "sess_legacy",
        "workspace_root": "/tmp",
        "created_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    assert!(header.team);
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
        crate::model::SessionToggles {
            web_search: false,
            ..Default::default()
        },
    );
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let (mut task, _rx) = task_with(session, store);
    task.config = Arc::new(openai_config());

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
async fn submit_expands_at_mentions_into_user_parts() {
    let dir = tmp("mention");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.txt"), "mention body").unwrap();

    let session = Session::with_model(
        &dir,
        None,
        "openai/test_model",
        None,
        crate::model::SessionToggles {
            web_search: false,
            ..Default::default()
        },
    );
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let (mut task, _rx) = task_with(session, store);
    task.config = Arc::new(openai_config());

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

// ---- 程序化投递与轮次收尾 ----

fn queued(text: &str) -> (PendingDelivery, super::handle::DeliveryReceipt) {
    let (completion, receipt) = oneshot::channel();
    (
        PendingDelivery {
            text: text.into(),
            completion,
        },
        receipt,
    )
}

#[tokio::test]
async fn deliver_while_running_is_queued_and_acked() {
    let (mut task, _view) = event_task();
    let (tx, mut rx) = mpsc::channel(4);
    let (_events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(1);
    let (reply, reply_rx) = oneshot::channel();
    // 保持 tx 存活：发送端全部关闭会使 drive 视为会话退出
    let sender = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = sender
            .send(Command::Deliver {
                delivery: queued("hello").0,
                reply,
            })
            .await;
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
    cleanup(&task);
}

#[tokio::test]
async fn pending_deliveries_run_as_turns_and_bump_seq() {
    let (mut task, _view) = event_task();
    let seq_rx = task.turn_tx.subscribe();
    // 不可达端点：投递轮次快速失败（不出本机），验证冲刷机制而非补全结果
    task.config = Arc::new(unreachable_config());
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
        assert!(!task.finish_turn(outcome).await);
        assert!(receipt.await.unwrap().is_err());
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
    super::manager::SessionBrokerHandle,
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
    let broker = super::manager::SessionBrokerHandle::new(manager.clone());
    (dir, manager, parent, broker)
}

fn agent_def(name: &str) -> crate::agents::AgentDef {
    crate::agents::AgentDef {
        name: name.into(),
        description: "test".into(),
        model: None,
        tools: vec!["Read".into()],
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

/// 投递回复先于 WaitSession 完成时，等待仍返回对应回执。
#[tokio::test]
async fn wait_turn_returns_delivered_reply_after_fast_completion() {
    let (dir, manager, parent, broker) = subagent_fixture().await;
    let launch = broker
        .spawn_subagent(
            &SubagentContext::from_session(&parent.snapshot().session),
            &agent_def("general"),
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

// ---- 会话前导与只追加提示 ----

fn persisted_task() -> SessionTask {
    let (mut task, _view) = event_task();
    task.view.persisted = true;
    task
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
        crate::model::SessionToggles {
            web_search: false,
            ..Default::default()
        },
    );
    let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
    let (mut task, _rx) = task_with(session, store);
    task.config = Arc::new(openai_config());
    task.preamble = Some(Arc::new(|_| {
        vec!["<system-reminder type=\"memory\">\nM\n</system-reminder>".into()]
    }));

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
async fn note_when_idle_appends_and_persists_without_a_turn() {
    let mut task = persisted_task();
    let id = task.view.session.header.id.clone();
    task.store.save_header(&task.view.session).await.unwrap();
    task.note("N1".into()).await.unwrap();
    let turns = &task.view.session.turns;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].role, Role::User);
    assert_eq!(turns[0].text_content(), "N1");
    assert!(matches!(task.view.state, RunState::Idle));
    let stored = task.store.load_session(&id).await.unwrap().unwrap();
    assert_eq!(stored.turns.len(), 1);
    cleanup(&task);

    let (mut fresh, _view) = event_task();
    assert!(fresh.note("x".into()).await.is_err());
    cleanup(&fresh);
}

#[tokio::test]
async fn note_while_running_is_queued_then_appended_in_order() {
    let mut task = persisted_task();
    task.store.save_header(&task.view.session).await.unwrap();
    let (tx, mut rx) = mpsc::channel(4);
    let (_events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(1);
    let (reply, reply_rx) = oneshot::channel();
    let sender = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = sender
            .send(Command::Note {
                text: "N1".into(),
                reply,
            })
            .await;
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
    // 运行中只入队：历史不变、不起轮次
    assert!(task.view.session.turns.is_empty());
    assert_eq!(task.pending_notes, vec!["N1".to_string()]);
    assert_eq!(task.turn_tx.borrow().seq, 0);

    // 下一次模型调用前追加（位于助手回复之前），之后到达的在收尾追加
    let (_tx2, mut rx2) = mpsc::channel(1);
    task.execute(engine(done_provider(), Default::default()), &mut rx2)
        .await
        .unwrap();
    task.pending_notes.push("N2".into());
    assert!(!task.finish_turn(Ok(TurnOutcome::Completed)).await);
    let texts: Vec<_> = task
        .view
        .session
        .turns
        .iter()
        .map(|t| (t.role, t.text_content()))
        .collect();
    assert_eq!(
        texts,
        vec![
            (Role::User, "N1".into()),
            (Role::Assistant, "done".into()),
            (Role::User, "N2".into()),
        ]
    );
    cleanup(&task);
}
