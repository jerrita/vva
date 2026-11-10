use super::session_shared::*;
use crate::{
    harness::{
        session::{
            handle::{Command, PendingDelivery},
            task::*,
        },
        *,
    },
    model::Session,
    storage::{SessionStore, jsonl::JsonlSessionStore},
    tools::background::BackgroundStatus,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};
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
    vva_core::harness::session::handle::DeliveryReceipt,
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
    let session = Session::with_model(&root, Some("Async fixture".into()), "p/m", None);
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
