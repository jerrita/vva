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
    Gate {
        permits: Arc<tokio::sync::Semaphore>,
        finished: Arc<tokio::sync::Semaphore>,
    },
}

#[async_trait::async_trait]
impl crate::tools::Tool for TestTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            serde_json::json!({"type": "object", "properties": {"label": {"type": "string"}}})
        })
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
            TestBlock::Gate { permits, finished } => {
                permits.acquire().await.unwrap().forget();
                finished.add_permits(1);
                Ok(arguments.into())
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
async fn curation_tool_persists_running_session_flag_and_reports_write_failure() {
    let (mut task, view) = event_task();
    task.config = Arc::new(unreachable_config());
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
        (crate::model::SessionKind::Oneshot, true, None, false),
        (crate::model::SessionKind::Curator, true, None, false),
    ] {
        let (mut task, _view) = event_task();
        task.config = Arc::new(unreachable_config());
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
    task.queue_task_completion(id.clone()).await;
    assert!(task.view.notices.is_empty());
    assert_eq!(
        task.view.background_tasks[&id].status,
        crate::model::BackgroundTaskStatus::Completed
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
    for expected in [
        format!("Background task {id} exited with code 0").as_str(),
        "done",
        "TaskOutput",
    ] {
        assert!(text.contains(expected), "{text}");
    }
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

// ---- 工具执行 ----

#[tokio::test]
async fn launch_receipt_does_not_regress_early_completion_and_reload_interrupts_running_tasks() {
    use crate::model::{BackgroundTask, BackgroundTaskStatus, ToolResult};
    let (mut task, _) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    let completed = BackgroundTask {
        status: BackgroundTaskStatus::Completed,
        output: "ready".into(),
        ..BackgroundTask::running("task_sample".into())
    };
    task.event(AgentEvent::BackgroundTaskUpdated(completed.clone()))
        .await;
    let mut result = ToolResult::success("call_sample", "started");
    result.background_task_id = Some(completed.task_id.clone());
    result.task = Some(BackgroundTask::running(completed.task_id.clone()));
    task.append(Turn::new(Role::Tool, vec![Message::tool_result(result)]))
        .await
        .unwrap();
    assert_eq!(task.view.background_tasks[&completed.task_id], completed);
    let restored = SessionView::new((*task.view.session).clone(), true);
    assert_eq!(
        restored.background_tasks[&completed.task_id].status,
        BackgroundTaskStatus::Interrupted
    );
    cleanup(&task);
}

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

#[tokio::test]
async fn steering_waits_for_the_complete_parallel_tool_batch() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut task, handle, mut commands) = command_task();
        task.store.save_header(&task.view.session).await.unwrap();
        let permits = Arc::new(tokio::sync::Semaphore::new(0));
        let finished = Arc::new(tokio::sync::Semaphore::new(0));
        let tools = registry_with(TestTool {
            name: "Gate",
            blocking: TestBlock::Gate {
                permits: permits.clone(),
                finished: finished.clone(),
            },
        });
        let provider = ScriptedProvider::new(|repeat| {
            if repeat {
                return vec![Message::text("completed")];
            }
            vec![Message::new(
                ["tool_one", "tool_two"]
                    .map(|id| ContentPart::ToolCall(ToolCall::new(id, "Gate", "{}")))
                    .into(),
            )]
        });
        let (events, mut output) = mpsc::channel(64);
        task.cli = Some(events);
        let control = async {
            while !matches!(output.recv().await, Some(AgentEvent::ToolExecuting { .. })) {}
            handle
                .submit("guidance between requests", vec![], None)
                .await
                .unwrap();
            let id = handle.snapshot().pending_inputs[0].id.clone();
            handle.set_input_mode(id, InputMode::Steer).await.unwrap();
            permits.add_permits(1);
            finished.acquire().await.unwrap().forget();
            assert!(
                !handle
                    .snapshot()
                    .session
                    .turns
                    .iter()
                    .any(|turn| turn.steering)
            );
            assert_eq!(handle.snapshot().pending_inputs.len(), 1);
            permits.add_permits(1);
        };
        let (outcome, ()) = tokio::join!(
            task.execute(engine(provider, tools), &mut commands),
            control
        );
        assert_eq!(outcome.unwrap(), TurnOutcome::Completed);
        let turns = &task.view.session.turns;
        let guide = turns.iter().position(|turn| turn.steering).unwrap();
        assert_eq!(
            turns[..guide]
                .iter()
                .filter(|turn| turn.role == Role::Tool)
                .count(),
            2
        );
        assert!(turns.last().unwrap().worked_ms.is_some());
        assert!(task.view.pending_inputs.is_empty());
        cleanup(&task);
    })
    .await
    .expect("parallel steering timed out");
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
    cleanup(&task);
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
    task.config = Arc::new(openai_config());

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
    assert!(changed[0].contains("Added:\n- beta: Beta skill."));
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
    let broker = super::manager::SessionBrokerHandle::new(manager.clone(), parent.id.clone());
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
        crate::model::SessionToggles { web_search: false },
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

fn command_task() -> (SessionTask, Arc<ManagedSession>, mpsc::Receiver<Command>) {
    let root = tmp("async-session");
    let session = Session::with_model(
        &root,
        Some("Async fixture".into()),
        "p/m",
        None,
        Default::default(),
    );
    let id = session.header.id.clone();
    let (commands, rx) = mpsc::channel(1);
    let (task, view, turns) = SessionTask::new(
        session,
        true,
        TaskDeps {
            store: Arc::new(JsonlSessionStore::new(&root).unwrap()),
            config: Arc::new(unreachable_config()),
            state_path: root.join("state.json"),
            broker: Arc::new(StubBroker(None)),
            tool_factory: None,
            preamble: None,
        },
        commands.downgrade(),
    );
    let handle = Arc::new(ManagedSession {
        id,
        commands,
        view,
        turns,
    });
    (task, handle, rx)
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
    task.config = Arc::new(config);
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

fn async_report(text: &str) -> SubagentReport {
    SubagentReport {
        session_id: "sess_worker".into(),
        agent: "general".into(),
        title: String::new(),
        turn_seq: 1,
        text: text.into(),
        error: None,
    }
}

#[tokio::test]
async fn user_queue_steers_before_finalizing_and_runs_after_messages_in_order() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (config, mut requests, server) =
            completion_endpoint_controlled(Some(gate.clone())).await;
        let (mut task, handle, commands) = command_task();
        task.config = Arc::new(config);
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
        task.config = Arc::new(config);
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
        task.config = Arc::new(config);
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
        task.config = Arc::new(config);
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
        let broker = super::manager::SessionBrokerHandle::new(manager.clone(), parent.id.clone());
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
            task.config = Arc::new(config.clone());
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

#[tokio::test]
async fn completion_persistence_failure_closes_output_and_reports_error() {
    let (mut task, _view) = event_task();
    task.queue_session_completion(async_report("fixture"), false)
        .await;
    let (cli, mut output) = mpsc::channel(64);
    task.cli = Some(cli);
    let (delivery, receipt) = queued("fixture");
    task.active_deliveries.push(delivery);
    // 尚无会话文件，持久化失败必须结束本次输出并解除回执。
    task.finish_turn(Ok(TurnOutcome::Completed)).await;
    assert!(receipt.await.unwrap().is_err());
    assert!(task.turn_tx.borrow().settled);
    assert!(task.turn_tx.borrow().error.is_some());
    assert!(!task.auto_resume);
    let mut reported = false;
    while let Some(event) = output.recv().await {
        if matches!(event, AgentEvent::Error(_)) {
            reported = true;
        }
    }
    assert!(reported);
    assert_eq!(task.subagent_completions.len(), 1);
    cleanup(&task);
}

#[tokio::test]
async fn idle_shell_output_publishes_without_model_calls_and_ignores_removed_calls() {
    use tokio::io::AsyncWriteExt;
    let (mut task, handle, commands) = command_task();
    task.auto_resume = false;
    let root = task.view.session.header.workspace_root.clone();
    task.store.save_header(&task.view.session).await.unwrap();
    task.append(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::ToolCall(ToolCall::new("live", "Shell", "{}")),
    ])]))
    .await
    .unwrap();
    task.shell_calls.insert("live".into());
    let capture = task.shell_outputs.capture("live");
    let stale = task.shell_outputs.capture("removed");
    let outputs = task.shell_outputs.clone();
    let (pipe, mut writer) = tokio::io::duplex(64);
    let output_handle = capture.handle.clone();
    let reader = tokio::spawn(async move {
        crate::tools::shell_output::read_output(pipe, Some(&output_handle), None, |_| {}).await
    });
    let actor = tokio::spawn(task.run(commands));
    writer.write_all(b"without newline").await.unwrap();
    let mut view = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if view
                .borrow()
                .shell_outputs
                .get("live")
                .is_some_and(|out| out.text == "without newline")
            {
                break;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(matches!(view.borrow().state, RunState::Idle));
    assert!(!view.borrow().shell_outputs.contains_key("removed"));
    drop(capture);
    drop(stale);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if view.borrow().shell_outputs["live"].finished {
                break;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(view.borrow().shell_outputs["live"].text, "without newline");
    drop(writer);
    reader.await.unwrap().unwrap();
    drop(outputs);
    drop(handle);
    actor.await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[tokio::test]
async fn aborted_shell_retains_streamed_output_in_paired_result_and_reload() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let (mut task, mut view) = event_task();
        task.store.save_header(&task.view.session).await.unwrap();
        task.view.persisted = true;
        let tool = crate::tools::shell::ShellTool::new(&task.view.session.header.workspace_root)
            .with_outputs(Some(task.shell_outputs.clone()), None);
        let mut tools = crate::tools::ToolRegistry::new();
        tools.register(Arc::new(tool));
        let provider = ScriptedProvider::new(|_| {
            vec![Message::new(vec![ContentPart::ToolCall(ToolCall::new(
                "synthetic-stream",
                "Shell",
                r#"{"intent":"Stream fixture","command":"printf fixture; exec sleep 60"}"#,
            ))])]
        });
        let (tx, mut commands) = mpsc::channel(1);
        let actor = tokio::spawn(async move {
            let result = task.execute(engine(provider, tools), &mut commands).await;
            (task, result)
        });
        loop {
            if view
                .borrow()
                .shell_outputs
                .get("synthetic-stream")
                .is_some_and(|output| output.text == "fixture")
            {
                break;
            }
            view.changed().await.unwrap();
        }
        let (reply, _) = oneshot::channel();
        tx.send(Command::Stop {
            delete: false,
            reply,
        })
        .await
        .unwrap();
        let (task, result) = actor.await.unwrap();
        assert_eq!(result.unwrap(), TurnOutcome::Aborted);
        let loaded = task
            .store
            .load_session(&task.view.session.header.id)
            .await
            .unwrap()
            .unwrap();
        let restored = SessionView::new(loaded, true);
        let output = &restored.shell_outputs["synthetic-stream"];
        assert_eq!(output.text, "fixture");
        assert!(output.finished);
        assert!(output.error.is_some());
        cleanup(&task);
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn idle_shell_output_coalesces_bursts_and_remains_quiet_without_new_data() {
    let (mut task, handle, commands) = command_task();
    task.auto_resume = false;
    task.shell_calls.insert("synthetic".into());
    let root = task.view.session.header.workspace_root.clone();
    let capture = task.shell_outputs.capture("synthetic");
    let mut decoder = crate::tools::shell_output::OutputDecoder::default();
    let mut view = handle.subscribe();
    view.borrow_and_update();
    let actor = tokio::spawn(task.run(commands));
    for _ in 0..100 {
        decoder.push(&capture.handle, b"x", false);
    }
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(99)).await;
    tokio::task::yield_now().await;
    assert!(!view.has_changed().unwrap());
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        view.borrow_and_update().shell_outputs["synthetic"]
            .text
            .len(),
        100
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert!(!view.has_changed().unwrap());
    decoder.push(&capture.handle, b"next", false);
    tokio::task::yield_now().await;
    assert_eq!(
        view.borrow_and_update().shell_outputs["synthetic"]
            .text
            .len(),
        104
    );
    drop(capture);
    drop(handle);
    actor.await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}
