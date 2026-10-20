//! Shell 后台任务：detached 执行、输出捕获与生命周期管理。
//! 任务为内存态、随会话结束回收；完成经 CompletionSink 回投会话，
//! 由 SessionTask 注入用户轮次并自动续跑。

use async_trait::async_trait;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;

use super::parse_args;
use super::{Tool, TruncateSide, truncate_utf8};

/// 单个任务的输出保留上限（超出丢弃头部，仅保尾部）
pub const BACKGROUND_BUFFER_CAP: usize = 256 * 1024;
/// TaskOutput 返回的尾部字节数
pub const TASK_OUTPUT_TAIL: usize = 16 * 1024;
/// 完成通知轮次内联的尾部字节数
pub const NOTIFY_TAIL_BYTES: usize = 4 * 1024;

/// 任务未找到时的统一提示：任务不随会话重启或分叉存活
const NOT_FOUND: &str = "not found; tasks do not survive session restarts or forks";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundStatus {
    Running,
    /// TaskStop / 会话回收终止
    Killed,
    /// code 为 None 表示被信号终止
    Exited {
        code: Option<i32>,
    },
}

pub fn status_line(status: &BackgroundStatus) -> String {
    match status {
        BackgroundStatus::Running => "running".to_string(),
        BackgroundStatus::Killed => "stopped by TaskStop".to_string(),
        BackgroundStatus::Exited { code: Some(code) } => format!("exited with code {code}"),
        BackgroundStatus::Exited { code: None } => "exited (killed by signal)".to_string(),
    }
}

/// stdout 与 stderr 合并捕获的 keep-tail 缓冲
#[derive(Default)]
struct TaskOutputBuffer {
    data: Vec<u8>,
    /// 累计捕获字节数（含已丢弃部分），供模型判断是否有新输出
    total: u64,
}

impl TaskOutputBuffer {
    fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        self.data.extend_from_slice(chunk);
        if self.data.len() > BACKGROUND_BUFFER_CAP {
            let excess = self.data.len() - BACKGROUND_BUFFER_CAP;
            self.data.drain(..excess);
        }
    }

    fn truncated(&self) -> bool {
        self.total > self.data.len() as u64
    }
}

/// monitor 与 reader 共享的任务状态（注册表锁外可独立更新）
struct TaskInner {
    status: BackgroundStatus,
    output: TaskOutputBuffer,
}

struct BackgroundTask {
    intent: String,
    command: String,
    inner: Arc<Mutex<TaskInner>>,
    monitor: Option<tokio::task::JoinHandle<()>>,
}

/// 供 UI 与工具读取的只读快照
pub struct TaskSnapshot {
    pub id: String,
    pub intent: String,
    pub command: String,
    pub status: BackgroundStatus,
    pub total_bytes: u64,
    pub truncated: bool,
    /// 已按 TASK_OUTPUT_TAIL 截尾、UTF-8 安全的输出
    pub tail: String,
}

/// 完成回投：参数为 task id。tools 层不感知 harness 的 Command 协议，
/// 由 manager 关闭 WeakSender 构造（Title 任务同款通道）。
pub type CompletionSink = Arc<dyn Fn(String) + Send + Sync>;

pub struct BackgroundTasks {
    tasks: Mutex<BTreeMap<String, BackgroundTask>>,
    next_id: AtomicU64,
    sink: CompletionSink,
}

impl BackgroundTasks {
    pub fn new(sink: CompletionSink) -> Self {
        Self {
            tasks: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            sink,
        }
    }

    /// 后台启动命令，立即返回任务 id。子进程 kill_on_drop：
    /// monitor 中止（TaskStop / 会话回收）或运行时回收时兜底杀进程。
    pub fn spawn(&self, intent: &str, command: &str, working_dir: &Path) -> Result<String, String> {
        let mut cmd = super::shell::workspace_shell_command(command, working_dir)?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to run command: {e}"))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let inner = Arc::new(Mutex::new(TaskInner {
            status: BackgroundStatus::Running,
            output: TaskOutputBuffer::default(),
        }));
        self.tasks
            .lock()
            .expect("background tasks lock poisoned")
            .insert(
                id.clone(),
                BackgroundTask {
                    intent: intent.to_string(),
                    command: command.to_string(),
                    inner: inner.clone(),
                    monitor: None,
                },
            );
        let stdout = read_output(child.stdout.take(), inner.clone());
        let stderr = read_output(child.stderr.take(), inner.clone());
        let sink = self.sink.clone();
        let monitor_id = id.clone();
        let monitor = tokio::spawn(async move {
            // 读取 future 随 monitor 取消回收；正常退出时等管道读尽再落终态。
            let (exit, (), ()) = tokio::join!(child.wait(), stdout, stderr);
            // TaskStop / 会话回收已置 Killed 且结果已内联或会话已退出：不再回投
            let notify = {
                let mut inner = inner.lock().expect("task state lock poisoned");
                if inner.status == BackgroundStatus::Killed {
                    false
                } else {
                    inner.status = BackgroundStatus::Exited {
                        code: exit.ok().and_then(|status| status.code()),
                    };
                    true
                }
            };
            if notify {
                sink(monitor_id);
            }
        });
        self.tasks
            .lock()
            .expect("background tasks lock poisoned")
            .get_mut(&id)
            .expect("task entry just inserted")
            .monitor = Some(monitor);
        Ok(id)
    }

    pub fn snapshot(&self, id: &str) -> Option<TaskSnapshot> {
        let (intent, command, inner) = {
            let tasks = self.tasks.lock().expect("background tasks lock poisoned");
            let task = tasks.get(id)?;
            (
                task.intent.clone(),
                task.command.clone(),
                task.inner.clone(),
            )
        };
        let inner = inner.lock().expect("task state lock poisoned");
        Some(TaskSnapshot {
            id: id.to_string(),
            intent,
            command,
            status: inner.status.clone(),
            total_bytes: inner.output.total,
            truncated: inner.output.truncated(),
            tail: truncate_utf8(&inner.output.data, TASK_OUTPUT_TAIL, TruncateSide::Tail),
        })
    }

    /// 停止运行中的任务：置 Killed 并中止 monitor，wait future 被 drop
    /// 后由 kill_on_drop 杀掉子进程。已终态或未知 id 显式报错。
    pub fn stop(&self, id: &str) -> Result<String, String> {
        let mut tasks = self.tasks.lock().expect("background tasks lock poisoned");
        let Some(task) = tasks.get_mut(id) else {
            return Err(format!("Background task {id} {NOT_FOUND}"));
        };
        let mut inner = task.inner.lock().expect("task state lock poisoned");
        if inner.status != BackgroundStatus::Running {
            return Err(format!(
                "Background task {id} already {}",
                status_line(&inner.status)
            ));
        }
        inner.status = BackgroundStatus::Killed;
        drop(inner);
        if let Some(monitor) = task.monitor.take() {
            monitor.abort();
        }
        Ok(format!("Task {id} stopped."))
    }

    /// 会话结束回收：杀掉仍在运行的任务并清空注册表
    pub fn shutdown(&self) {
        let mut tasks = self.tasks.lock().expect("background tasks lock poisoned");
        for task in tasks.values_mut() {
            {
                let mut inner = task.inner.lock().expect("task state lock poisoned");
                if inner.status == BackgroundStatus::Running {
                    inner.status = BackgroundStatus::Killed;
                }
            }
            if let Some(monitor) = task.monitor.take() {
                monitor.abort();
            }
        }
        tasks.clear();
    }
}

async fn read_output(pipe: Option<impl AsyncReadExt + Unpin>, inner: Arc<Mutex<TaskInner>>) {
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => inner
                .lock()
                .expect("task state lock poisoned")
                .output
                .push(&buf[..n]),
        }
    }
}

/// 完成通知轮次正文：以 background-task reminder 注入会话，
/// 首行为状态（UI 展示用），其余为模型可见的输出详情
pub fn format_notification(snapshot: &TaskSnapshot) -> String {
    let mut notification = format!(
        "Background task {} {}\nintent: {}\ncommand: {}\ncaptured output: {} bytes",
        snapshot.id,
        status_line(&snapshot.status),
        snapshot.intent,
        snapshot.command,
        snapshot.total_bytes,
    );
    if snapshot.truncated {
        notification.push_str(&format!(
            " (buffer keeps last {BACKGROUND_BUFFER_CAP} bytes; earlier output discarded)"
        ));
    }
    let tail = truncate_utf8(
        snapshot.tail.as_bytes(),
        NOTIFY_TAIL_BYTES,
        TruncateSide::Tail,
    );
    if !tail.is_empty() {
        notification.push_str(&format!(
            "\n--- tail (last {NOTIFY_TAIL_BYTES} bytes) ---\n{tail}"
        ));
    }
    notification.push_str(&format!(
        "\nUse the TaskOutput tool with task_id \"{}\" to read more output.",
        snapshot.id
    ));
    crate::system::format_background_task_reminder(&notification)
}

#[derive(Deserialize)]
struct TaskIdArgs {
    task_id: String,
}

/// 查询后台任务状态与捕获输出
pub struct TaskOutputTool {
    tasks: Arc<BackgroundTasks>,
}

impl TaskOutputTool {
    pub fn new(tasks: Arc<BackgroundTasks>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "TaskOutput"
    }

    fn description(&self) -> &str {
        "Check the status and captured output of a background task started with Shell run_in_background. Returns the status, total bytes captured, and the most recent output; a changed byte count means new output arrived since the last check."
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task id returned by the background Shell call." }
                },
                "required": ["task_id"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: TaskIdArgs = parse_args(arguments, "TaskOutput")?;
        let snapshot = self
            .tasks
            .snapshot(&args.task_id)
            .ok_or_else(|| format!("Background task {} {NOT_FOUND}", args.task_id))?;
        let mut text = format!(
            "Task {}: {}\nintent: {}\ncommand: {}\noutput: {} bytes captured",
            snapshot.id,
            status_line(&snapshot.status),
            snapshot.intent,
            snapshot.command,
            snapshot.total_bytes,
        );
        if snapshot.truncated {
            text.push_str(&format!(
                " (buffer keeps last {BACKGROUND_BUFFER_CAP} bytes; earlier output discarded)"
            ));
        }
        text.push_str(&format!(
            "\n--- last {TASK_OUTPUT_TAIL} bytes ---\n{}",
            snapshot.tail
        ));
        Ok(text)
    }
}

/// 停止运行中的后台任务
pub struct TaskStopTool {
    tasks: Arc<BackgroundTasks>,
    broker: Option<Arc<dyn super::agent::SessionBroker>>,
}

impl TaskStopTool {
    pub fn new(tasks: Arc<BackgroundTasks>) -> Self {
        Self {
            tasks,
            broker: None,
        }
    }

    /// 绑定会话编排接口：task_id 命中 sess_ 会话 id 命名空间时路由到会话中止
    pub fn with_broker(mut self, broker: Arc<dyn super::agent::SessionBroker>) -> Self {
        self.broker = Some(broker);
        self
    }
}

#[async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "TaskStop"
    }

    fn description(&self) -> &str {
        if self.broker.is_some() {
            "Stop a running background task started with Shell run_in_background, or abort the \
             current turn of a session (e.g. a background subagent) by passing its sess_ id as \
             task_id. Stopped tasks do not produce a completion notification."
        } else {
            "Stop a running background task started with Shell run_in_background. \
             Stopped tasks do not produce a completion notification."
        }
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            let task_id = if self.broker.is_some() {
                "Task id returned by the background Shell call, or a sess_ session id."
            } else {
                "Task id returned by the background Shell call."
            };
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": task_id}
                },
                "required": ["task_id"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: TaskIdArgs = parse_args(arguments, "TaskStop")?;
        if args.task_id.starts_with("sess_") {
            let broker = self
                .broker
                .as_ref()
                .ok_or_else(|| "Session control is not available in this context".to_string())?;
            broker.abort_session(&args.task_id).await?;
            return Ok(format!("Session {} turn aborted.", args.task_id));
        }
        self.tasks.stop(&args.task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn recording_sink() -> (CompletionSink, Arc<Mutex<Vec<String>>>) {
        let ids = Arc::new(Mutex::new(Vec::new()));
        let captured = ids.clone();
        let sink: CompletionSink = Arc::new(move |id| captured.lock().unwrap().push(id));
        (sink, ids)
    }

    async fn wait_for_status(
        tasks: &BackgroundTasks,
        id: &str,
        want: &BackgroundStatus,
    ) -> TaskSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = tasks.snapshot(id).expect("task present");
            if &snapshot.status == want {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "status stuck at {:?}",
                snapshot.status
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn sleep_command() -> &'static str {
        if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        }
    }

    #[test]
    fn buffer_keeps_tail_and_counts_total() {
        let mut buffer = TaskOutputBuffer::default();
        let head = vec![b'a'; BACKGROUND_BUFFER_CAP];
        buffer.push(&head);
        buffer.push(b"tail-bytes");
        assert_eq!(buffer.data.len(), BACKGROUND_BUFFER_CAP);
        assert_eq!(&buffer.data[BACKGROUND_BUFFER_CAP - 10..], b"tail-bytes");
        assert_eq!(buffer.total, (BACKGROUND_BUFFER_CAP + 10) as u64);
        assert!(buffer.truncated());
    }

    #[test]
    fn tail_string_respects_char_boundaries() {
        // 多字节字符跨截断点时不 panic 且不产生半个字符
        let data: Vec<u8> = "好".repeat(4096).into_bytes();
        let tail = truncate_utf8(&data, TASK_OUTPUT_TAIL, TruncateSide::Tail);
        assert!(tail.len() <= TASK_OUTPUT_TAIL + 3);
        assert!(tail.chars().all(|ch| ch == '好'));
    }

    #[tokio::test]
    async fn spawn_captures_exit_code_and_merged_output() {
        let (sink, ids) = recording_sink();
        let tasks = BackgroundTasks::new(sink);
        let command = if cfg!(windows) {
            "Write-Output hi; [Console]::Error.WriteLine('err'); exit 3"
        } else {
            "echo hi; echo err >&2; exit 3"
        };
        let id = tasks
            .spawn("check", command, Path::new("."))
            .expect("spawn");

        let snapshot =
            wait_for_status(&tasks, &id, &BackgroundStatus::Exited { code: Some(3) }).await;
        assert_eq!(ids.lock().unwrap().as_slice(), std::slice::from_ref(&id));
        assert_eq!(snapshot.intent, "check");
        assert!(snapshot.tail.contains("hi"), "tail: {}", snapshot.tail);
        assert!(snapshot.tail.contains("err"), "tail: {}", snapshot.tail);
        assert!(!snapshot.truncated);
    }

    #[tokio::test]
    async fn stop_kills_running_task_without_notification() {
        let (sink, ids) = recording_sink();
        let tasks = BackgroundTasks::new(sink);
        let id = tasks
            .spawn("long", sleep_command(), Path::new("."))
            .unwrap();
        assert_eq!(
            tasks.snapshot(&id).unwrap().status,
            BackgroundStatus::Running
        );

        let message = tasks.stop(&id).unwrap();
        assert_eq!(message, format!("Task {id} stopped."));
        assert_eq!(
            tasks.snapshot(&id).unwrap().status,
            BackgroundStatus::Killed
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(ids.lock().unwrap().is_empty(), "TaskStop must not notify");
    }

    #[tokio::test]
    async fn cancellation_closes_pending_output_pipes() {
        for stop in [true, false] {
            let (sink, ids) = recording_sink();
            let tasks = BackgroundTasks::new(sink);
            let inner = Arc::new(Mutex::new(TaskInner {
                status: BackgroundStatus::Running,
                output: TaskOutputBuffer::default(),
            }));
            let (stdout, mut stdout_peer) = tokio::io::duplex(64);
            let (stderr, mut stderr_peer) = tokio::io::duplex(64);
            let stdout = read_output(Some(stdout), inner.clone());
            let stderr = read_output(Some(stderr), inner.clone());
            let monitor = tokio::spawn(async move {
                tokio::join!(stdout, stderr);
            });
            tasks.tasks.lock().unwrap().insert(
                "1".into(),
                BackgroundTask {
                    intent: "capture output".into(),
                    command: String::new(),
                    inner,
                    monitor: Some(monitor),
                },
            );
            tokio::task::yield_now().await;

            if stop {
                tasks.stop("1").unwrap();
            } else {
                tasks.shutdown();
            }

            // 保持另一端打开，模拟继承了输出管道的进程仍在运行。
            for peer in [&mut stdout_peer, &mut stderr_peer] {
                let mut byte = [0];
                let read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut byte))
                    .await
                    .expect("cancelled monitor must release its output pipes")
                    .unwrap();
                assert_eq!(read, 0);
            }
            assert!(ids.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn stop_reports_unknown_and_finished_tasks() {
        let (sink, _ids) = recording_sink();
        let tasks = BackgroundTasks::new(sink);
        let error = tasks.stop("404").unwrap_err();
        assert!(error.contains("not found"), "{error}");

        let id = tasks
            .spawn(
                "quick",
                if cfg!(windows) { "exit 0" } else { "true" },
                Path::new("."),
            )
            .unwrap();
        wait_for_status(&tasks, &id, &BackgroundStatus::Exited { code: Some(0) }).await;
        let error = tasks.stop(&id).unwrap_err();
        assert!(error.contains("already exited with code 0"), "{error}");
    }

    #[tokio::test]
    async fn shutdown_kills_all_and_clears_registry() {
        let (sink, _ids) = recording_sink();
        let tasks = BackgroundTasks::new(sink);
        let first = tasks.spawn("one", sleep_command(), Path::new(".")).unwrap();
        let second = tasks.spawn("two", sleep_command(), Path::new(".")).unwrap();

        tasks.shutdown();
        assert!(tasks.snapshot(&first).is_none());
        assert!(tasks.snapshot(&second).is_none());
        assert!(tasks.stop(&first).is_err());
    }
}
