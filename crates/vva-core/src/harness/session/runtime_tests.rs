use super::{task::*, *};
use crate::{
    harness::{FixedSetup, SessionSetup},
    model::*,
    test_support::session::*,
    tools::{agent::SubagentReport, background::BackgroundStatus},
};
async fn completion_endpoint() -> (
    Arc<dyn SessionSetup>,
    mpsc::UnboundedReceiver<serde_json::Value>,
    tokio::task::JoinHandle<()>,
) {
    completion_endpoint_controlled(None).await
}
async fn completion_endpoint_controlled(
    gate: Option<Arc<tokio::sync::Semaphore>>,
) -> (
    Arc<dyn SessionSetup>,
    mpsc::UnboundedReceiver<serde_json::Value>,
    tokio::task::JoinHandle<()>,
) {
    let (requests, rx) = mpsc::unbounded_channel();
    let (endpoint,server)=crate::test_support::http::serve(move |request|{let requests=requests.clone();let gate=gate.clone();async move{
        requests.send(request.body).unwrap();if let Some(gate)=gate{gate.acquire().await.unwrap().forget();}
        let event=serde_json::json!({"type":"response.completed","sequence_number":1,"response":{"id":"resp_fixture","object":"response","created_at":0,"status":"completed","error":null,"incomplete_details":null,"model":"m","store":false,"previous_response_id":null,"output":[{"type":"message","id":"msg_fixture","status":"completed","role":"assistant","content":[{"type":"output_text","text":"handled","annotations":[]}]}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2,"input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}}});
        crate::test_support::http::Response{status:200,headers:vec![("content-type","text/event-stream".into())],body:format!("data: {event}\n\n"),keep_open:false}
    }}).await;
    let mut plan = engine(done_provider(), Default::default());
    plan.provider = Arc::new(
        crate::provider::response::ResponseProvider::new("fixture")
            .with_base_url(endpoint)
            .with_websocket(false),
    );
    plan.options.model = "m".into();
    plan.inputs.modalities = vec!["text".into(), "image".into()];
    (Arc::new(FixedSetup::new(plan)), rx, server)
}
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
        vva_core::model::BackgroundTaskStatus::Completed
    );

    task.drain_task_completions().await.unwrap();
    assert!(task.task_completions.is_empty());
    let text = task.view.session.turns.last().unwrap().text_content();
    assert!(text.contains("type=\"background-task\""));
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
        vva_core::model::BackgroundTaskStatus::Completed
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
    assert!(turns[0].text_content().contains("background-task"));
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
    assert!(text.contains("type=\"background-task\""));
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
        vva_core::model::BackgroundTaskStatus::Failed
    );
    cleanup(&task);
}

// ---- 管理器：模型选择 / 回退 / 分叉 / 编辑器 ----

#[tokio::test]
async fn pending_deliveries_run_as_turns_and_bump_seq() {
    let (mut task, _view) = event_task();
    let seq_rx = task.turn_tx.subscribe();
    // 不可达端点：投递轮次快速失败（不出本机），验证冲刷机制而非补全结果
    task.setup = Arc::new(FixedSetup::new(ExecutionPlan {
        provider: Arc::new(
            crate::provider::response::ResponseProvider::new("fixture")
                .with_base_url("http://127.0.0.1:1")
                .with_websocket(false),
        ),
        ..engine(done_provider(), Default::default())
    }));
    Arc::make_mut(&mut task.view.session)
        .append_turn(Turn::new(Role::System, vec![Message::text("fixture")]));
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

#[tokio::test]
async fn programmatic_steering_continues_the_current_turn_and_preserves_after_delivery() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (config, mut requests, server) =
            completion_endpoint_controlled(Some(gate.clone())).await;
        let (mut task, handle, commands) = command_task();
        task.setup = config;
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
        task.setup = config;
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
        let (config, mut requests, server) = completion_endpoint_controlled(Some(gate)).await;

        let (mut task, handle, commands) = command_task();
        task.setup = config;
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
        task.setup = config;
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
        task.setup = config;
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
async fn shell_exit_resumes_idle_session_for_success_and_failure() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (config, mut requests, server) = completion_endpoint().await;
        for exit_code in [0, 7] {
            let (mut task, handle, commands) = command_task();
            task.setup = config.clone();
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
