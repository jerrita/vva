use super::task::*;
use super::*;
use crate::harness::{ExecutionPlan, FixedSetup, SessionSetup};
use crate::model::{
    Annotation, SearchStatus, ToolCall, WebSearchAction, WebSearchCall, WebSearchSource,
};
use crate::provider::{CompletionOptions, ProviderResponse};
use crate::storage::jsonl::JsonlSessionStore;
use crate::tools::agent::SubagentReport;
use crate::tools::background::BackgroundStatus;
use std::path::PathBuf;

// ---- 共享夹具 ----

pub(crate) fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vva-{tag}-{}", uuid::Uuid::new_v4()))
}

pub(crate) fn task_with(
    session: Session,
    store: Arc<dyn SessionStore>,
) -> (SessionTask, watch::Receiver<Arc<SessionView>>) {
    let setup = fixed_setup();
    let (commands, _rx) = mpsc::channel(1);
    let (task, view, _) = SessionTask::new(
        session,
        false,
        TaskDeps { store, setup },
        commands.downgrade(),
    );
    (task, view)
}

pub(crate) fn event_task() -> (SessionTask, watch::Receiver<Arc<SessionView>>) {
    let workspace = tmp("events");
    let store = Arc::new(JsonlSessionStore::new(&workspace).unwrap());
    task_with(Session::new(workspace, None), store)
}

/// 按调用次序产出响应的供应商桩：首次调用取 `script(false)`，之后取 `script(true)`
pub(crate) struct ScriptedProvider {
    pub(crate) script: fn(bool) -> Vec<Message>,
    pub(crate) called: std::sync::atomic::AtomicBool,
}

impl ScriptedProvider {
    pub(crate) fn new(script: fn(bool) -> Vec<Message>) -> Arc<Self> {
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

pub(crate) fn engine(
    provider: Arc<ScriptedProvider>,
    tools: crate::tools::ToolRegistry,
) -> ExecutionPlan {
    ExecutionPlan {
        provider,
        tools,
        options: CompletionOptions::default(),
        notices: Vec::new(),
        external_web: Vec::new(),
        inputs: Default::default(),
    }
}

pub(crate) fn done_provider() -> Arc<ScriptedProvider> {
    ScriptedProvider::new(|_| vec![Message::text("done")])
}

/// 测试工具：可选阻塞语义，参数原样回显
pub(crate) struct TestTool {
    pub(crate) name: &'static str,
    /// 等待栅栏 (并发验证) 或永不完成 (中止验证)
    pub(crate) blocking: TestBlock,
}

pub(crate) enum TestBlock {
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

pub(crate) fn registry_with(tool: TestTool) -> crate::tools::ToolRegistry {
    let mut tools = crate::tools::ToolRegistry::new();
    tools.register(Arc::new(tool));
    tools
}

/// 等待后台任务达到期望终态（真实子进程，轮询 snapshot）
pub(crate) async fn wait_for_task(task: &SessionTask, id: &str, want: BackgroundStatus) {
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
pub(crate) async fn task_with_finished_background(command: &str) -> (SessionTask, String) {
    let (task, _view) = event_task();
    task.store.save_header(&task.view.session).await.unwrap();
    let root = task.view.session.header.workspace_root.clone();
    let id = task.tasks.spawn("bg", command, &root).unwrap();
    wait_for_task(&task, &id, BackgroundStatus::Exited { code: Some(0) }).await;
    (task, id)
}

pub(crate) fn cleanup(task: &SessionTask) {
    let _ = std::fs::remove_dir_all(&task.view.session.header.workspace_root);
}

// ---- 程序化投递与轮次收尾 ----

pub(crate) fn queued(
    text: &str,
) -> (
    PendingDelivery,
    crate::harness::session::handle::DeliveryReceipt,
) {
    let (completion, receipt) = oneshot::channel();
    (
        PendingDelivery {
            text: text.into(),
            mode: InputMode::After,
            completion,
        },
        receipt,
    )
}

// ---- 会话前导与只追加提示 ----

pub(crate) fn persisted_task() -> SessionTask {
    let (mut task, _view) = event_task();
    task.view.persisted = true;
    task
}

pub(crate) fn command_task() -> (SessionTask, Arc<ManagedSession>, mpsc::Receiver<Command>) {
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
    let setup = fixed_setup();
    let (task, view, turns) = SessionTask::new(
        session,
        true,
        TaskDeps {
            store: Arc::new(JsonlSessionStore::new(&root).unwrap()),
            setup,
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

pub(crate) fn async_report(text: &str) -> SubagentReport {
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
async fn drain_skips_vanished_task_without_error() {
    let (mut task, _view) = event_task();
    task.task_completions.push("404".into());

    task.drain_task_completions().await.unwrap();

    assert!(task.view.session.turns.is_empty());
    assert!(task.task_completions.is_empty());
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
        for programmatic in [false, true] {
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
                let receipt = if programmatic {
                    let (mut delivery, receipt) = queued("guidance between requests");
                    delivery.mode = InputMode::Steer;
                    handle
                        .request(|reply| Command::Deliver { delivery, reply })
                        .await
                        .unwrap();
                    Some(receipt)
                } else {
                    handle
                        .submit("guidance between requests", vec![], None)
                        .await
                        .unwrap();
                    let id = handle.snapshot().pending_inputs[0].id.clone();
                    handle.set_input_mode(id, InputMode::Steer).await.unwrap();
                    None
                };
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
                assert_eq!(
                    handle.snapshot().pending_inputs.len(),
                    usize::from(!programmatic)
                );
                permits.add_permits(1);
                receipt
            };
            let (outcome, receipt) = tokio::join!(
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
            assert!(task.pending_deliveries.is_empty());
            if let Some(mut receipt) = receipt {
                assert!(matches!(
                    receipt.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
                assert!(!task.finish_turn(Ok(TurnOutcome::Completed)).await);
                assert_eq!(receipt.await.unwrap().unwrap().text, "completed");
            }
            cleanup(&task);
        }
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

fn fixed_setup() -> Arc<dyn SessionSetup> {
    Arc::new(FixedSetup::new(engine(done_provider(), Default::default())))
}

#[tokio::test]
async fn releasing_manager_closes_session_tasks() {
    let root = tmp("manager-lifecycle");
    let store = Arc::new(JsonlSessionStore::new(&root).unwrap());
    let manager = Arc::new(SessionManager::new(store));
    let handle = manager
        .create(crate::harness::AgentAssembly {
            session: Session::new(&root, None),
            setup: fixed_setup(),
        })
        .await
        .unwrap();
    let mut turns = handle.turn_receiver();
    let weak = Arc::downgrade(&manager);
    drop(handle);
    drop(manager);
    assert!(weak.upgrade().is_none());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), turns.changed())
            .await
            .unwrap()
            .is_err()
    );
    let _ = std::fs::remove_dir_all(root);
}
