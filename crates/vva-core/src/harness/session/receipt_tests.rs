use super::*;
#[tokio::test]
async fn cross_session_requests_keep_independent_receipts() {
    let root = crate::test_support::session::tmp("broker-receipts");
    let manager = Arc::new(SessionManager::new(Arc::new(
        crate::storage::jsonl::JsonlSessionStore::new(&root).unwrap(),
    )));
    let id = Session::new(&root, None).header.id;
    let (commands, mut requests) = mpsc::channel(4);
    let mut target_view = SessionView::new(Session::new(&root, None), false);
    target_view.state = RunState::Thinking;
    let (_, view) = watch::channel(Arc::new(target_view));
    let (_, turns) = watch::channel(CompletedTurn::default());
    manager.sessions.lock().await.as_mut().unwrap().insert(
        id.clone(),
        Arc::new(ManagedSession {
            id: id.clone(),
            commands,
            view,
            turns,
        }),
    );
    let target = manager.live(&id).await.unwrap();
    let mut callers = Vec::new();
    for _ in 0..2 {
        let session = Session::new(&root, None);
        let owner_id = session.header.id.clone();
        let (commands, rx) = mpsc::channel(1);
        let (_, view) = watch::channel(Arc::new(SessionView::new(session, false)));
        let (_, turns) = watch::channel(CompletedTurn::default());
        let handle = Arc::new(ManagedSession {
            id: owner_id.clone(),
            commands,
            view,
            turns,
        });
        manager
            .sessions
            .lock()
            .await
            .as_mut()
            .unwrap()
            .insert(owner_id.clone(), handle);
        callers.push((owner_id, rx));
    }
    let actor = tokio::spawn(async move {
        let mut deliveries = Vec::new();
        for _ in 0..2 {
            let Some(Command::Deliver { delivery, reply }) = requests.recv().await else {
                panic!("expected delivery");
            };
            let _ = reply.send(Ok(()));
            deliveries.push(delivery);
        }
        for delivery in deliveries {
            let report = crate::tools::agent::SubagentReport {
                session_id: id.clone(),
                agent: String::new(),
                title: String::new(),
                turn_seq: 2,
                error: None,
                text: delivery.text.clone(),
            };
            let _ = delivery.completion.send(Ok(report));
        }
    });
    let mut receipts = Vec::new();
    for (index, (broker, rx)) in callers.iter_mut().enumerate() {
        let text = format!("reply {index}");
        let owner = manager.live(broker).await.unwrap();
        let future = async {
            let receipt = target.deliver(&text).await?;
            owner
                .watch_delivery(target.id.clone(), receipt, false)
                .await
        };
        tokio::pin!(future);
        tokio::select! {
            _ = &mut future => panic!("registration must be acknowledged"),
            command = rx.recv() => {
                let Some(Command::WatchSession { receipt, background, reply, .. }) = command else { panic!("expected registration") };
                assert!(!background);
                receipts.push(receipt);
                let _ = reply.send(Ok(()));
            }
        }
        future.await.unwrap();
    }
    for (index, receipt) in receipts.into_iter().enumerate() {
        let report = receipt.await.unwrap().unwrap();
        assert_eq!(report.text, format!("reply {index}"));
    }
    actor.await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}
