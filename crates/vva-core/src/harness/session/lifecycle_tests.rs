use super::*;
use crate::{
    harness::{AgentAssembly, FixedSetup, SessionServices, SessionSetup},
    test_support::session::{ScriptedProvider, done_provider, engine},
    tools::{ask_user::AskUserTool, background::BackgroundTasks},
};

struct QuestionSetup {
    background: std::sync::Mutex<Option<Arc<BackgroundTasks>>>,
    title: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    title_started: tokio::sync::Semaphore,
}

#[async_trait::async_trait]
impl SessionSetup for QuestionSetup {
    async fn plan(&self, _: &Session, services: &SessionServices) -> Result<ExecutionPlan, String> {
        *self.background.lock().unwrap() = Some(services.background.clone());
        let provider = ScriptedProvider::new(|_| {
            vec![Message::new(vec![ContentPart::ToolCall(crate::model::ToolCall::new("ask","AskUser",serde_json::json!({"questions":[{"question":"Continue?","options":[{"label":"Yes","description":"Continue"}]}]}).to_string()))])]
        });
        let mut tools = crate::tools::ToolRegistry::new();
        tools.register(Arc::new(AskUserTool::new(
            services.ask_user_requests.clone(),
        )));
        Ok(engine(provider, tools))
    }
    async fn title(&self, _: &Session, _: &str) -> Result<Option<String>, String> {
        let _cancelled = self.title.lock().unwrap().take().unwrap();
        self.title_started.add_permits(1);
        std::future::pending().await
    }
}

#[tokio::test]
async fn close_and_shutdown_release_interaction_waiters_and_background_resources() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::storage::jsonl::JsonlSessionStore::new(root.path()).unwrap());
        let manager = Arc::new(SessionManager::new(store.clone()));
        let idle = manager
            .create(AgentAssembly {
                session: Session::new(root.path(), Some("idle".into())),
                setup: Arc::new(FixedSetup::new(engine(done_provider(), Default::default()))),
            })
            .await
            .unwrap();
        idle.deliver_wait("persisted history").await.unwrap();
        let (title, closed_title) = oneshot::channel();
        let setup = Arc::new(QuestionSetup {
            background: Default::default(),
            title: std::sync::Mutex::new(Some(title)),
            title_started: tokio::sync::Semaphore::new(0),
        });
        let active = manager
            .create(AgentAssembly {
                session: Session::new(root.path(), None),
                setup: setup.clone(),
            })
            .await
            .unwrap();
        let delivery = active.deliver("ask a question").await.unwrap();
        let mut views = active.subscribe();
        while views.borrow_and_update().pending_user_question.is_none() {
            views.changed().await.unwrap();
        }
        setup.title_started.acquire().await.unwrap().forget();
        let tasks = setup.background.lock().unwrap().clone().unwrap();
        let outputs = Arc::new(crate::tools::shell_output::ShellOutputs::default());
        let capture = outputs.capture("close-process");
        let output = capture.handle.clone();
        let id = tasks
            .spawn_captured(
                "keep running",
                if cfg!(windows) {
                    "Write-Output ready; Start-Sleep -Seconds 60"
                } else {
                    "printf ready; exec sleep 60"
                },
                root.path(),
                Some(capture),
                &HashMap::new(),
            )
            .unwrap();
        loop {
            outputs.drain();
            if output.snapshot().text.contains("ready") {
                break;
            }
            outputs.notified().await;
        }
        let (remote, receipt) = oneshot::channel();
        active
            .watch_delivery("remote".into(), receipt, true)
            .await
            .unwrap();
        manager.close(&active.id).await.unwrap();
        assert!(
            remote.is_closed(),
            "receipt waiter must be dropped before close returns"
        );
        assert!(closed_title.await.is_err(), "title future must be dropped");
        assert!(tasks.snapshot(&id).is_none());
        assert!(
            output.snapshot().finished,
            "background monitor must release its capture before close returns"
        );
        assert!(active.note("closed").await.is_err());
        assert!(
            views.has_changed().is_err(),
            "session task must have released its publisher"
        );
        assert!(delivery.await.unwrap().is_err());
        assert!(store.load_session(&active.id).await.unwrap().is_some());
        manager.shutdown().await.unwrap();
        assert!(idle.note("closed").await.is_err());
        assert!(store.load_session(&idle.id).await.unwrap().is_some());
        assert!(
            manager
                .create(AgentAssembly {
                    session: Session::new(root.path(), None),
                    setup: Arc::new(FixedSetup::new(engine(done_provider(), Default::default())))
                })
                .await
                .is_err()
        );
    })
    .await
    .expect("shutdown must complete without outstanding interactions");
}
