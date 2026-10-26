//! Shell 后台任务：detached 执行、输出捕获与生命周期管理。
//! 任务为内存态、随会话结束回收；完成经 CompletionSink 回投会话，
//! 由 SessionTask 注入用户轮次并自动续跑。

use super::shell_output::{OutputCapture, OutputHandle};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

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
    Failed {
        error: String,
    },
    /// code 为 None 表示被信号终止
    Exited {
        code: Option<i32>,
    },
}

pub fn status_line(status: &BackgroundStatus) -> String {
    match status {
        BackgroundStatus::Running => "running".to_string(),
        BackgroundStatus::Failed { error } => format!("failed: {error}"),
        BackgroundStatus::Killed => "stopped by TaskStop".to_string(),
        BackgroundStatus::Exited { code: Some(code) } => format!("exited with code {code}"),
        BackgroundStatus::Exited { code: None } => "exited (killed by signal)".to_string(),
    }
}

/// stdout 与 stderr 合并捕获的 keep-tail 缓冲
#[derive(Default)]
struct TaskOutputBuffer {
    data: VecDeque<u8>,
    /// 累计捕获字节数（含已丢弃部分），供模型判断是否有新输出
    total: u64,
}

impl TaskOutputBuffer {
    fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        let keep = BACKGROUND_BUFFER_CAP.saturating_sub(chunk.len());
        let excess = self.data.len().saturating_sub(keep);
        self.data.drain(..excess);
        self.data
            .extend(&chunk[chunk.len().saturating_sub(BACKGROUND_BUFFER_CAP)..]);
    }

    fn truncated(&self) -> bool {
        self.total > self.data.len() as u64
    }
}

/// monitor 与 reader 共享的任务状态（注册表锁外可独立更新）
struct TaskInner {
    status: BackgroundStatus,
    output: TaskOutputBuffer,
    pending: bool,
    shell_output: Option<OutputHandle>,
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
    pub shell_output: Option<crate::model::ShellOutput>,
}

impl TaskSnapshot {
    pub fn task(&self) -> crate::model::BackgroundTask {
        use crate::model::{BackgroundTask, BackgroundTaskStatus};
        let (status, exit_code) = match &self.status {
            BackgroundStatus::Running => (BackgroundTaskStatus::Running, None),
            BackgroundStatus::Failed { .. } => (BackgroundTaskStatus::Failed, None),
            BackgroundStatus::Killed => (BackgroundTaskStatus::Stopped, None),
            BackgroundStatus::Exited { code: Some(0) } => {
                (BackgroundTaskStatus::Completed, Some(0))
            }
            BackgroundStatus::Exited { code } => (BackgroundTaskStatus::Failed, *code),
        };
        BackgroundTask {
            task_id: self.id.clone(),
            status,
            output: self.tail.clone(),
            shell_output: self.shell_output.clone().map(Box::new),
            exit_code,
            total_bytes: Some(self.total_bytes),
            truncated: self.total_bytes > self.tail.len() as u64,
        }
    }
}

/// 完成回投：参数为 task id。tools 层不感知 harness 的 Command 协议，
/// 由 manager 关闭 WeakSender 构造（Title 任务同款通道）。
pub type CompletionSink = Arc<dyn Fn(String) + Send + Sync>;

pub struct BackgroundTasks {
    tasks: Mutex<BTreeMap<String, BackgroundTask>>,
    sink: CompletionSink,
}

impl BackgroundTasks {
    pub fn new(sink: CompletionSink) -> Self {
        Self {
            tasks: Mutex::new(BTreeMap::new()),
            sink,
        }
    }

    /// 后台启动命令，立即返回任务 id。子进程 kill_on_drop：
    /// monitor 中止（TaskStop / 会话回收）或运行时回收时兜底杀进程。
    pub fn spawn(&self, intent: &str, command: &str, working_dir: &Path) -> Result<String, String> {
        self.spawn_captured(intent, command, working_dir, None)
    }

    pub(crate) fn spawn_captured(
        &self,
        intent: &str,
        command: &str,
        working_dir: &Path,
        capture: Option<OutputCapture>,
    ) -> Result<String, String> {
        let child = (|| {
            let mut cmd = super::shell::workspace_shell_command(command, working_dir)?;
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            cmd.spawn()
                .map_err(|e| format!("Failed to run command: {e}"))
        })();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                if let Some(capture) = &capture {
                    capture.handle.finish(Some(error.clone()));
                }
                return Err(error);
            }
        };
        let id = format!("task_{}", uuid::Uuid::new_v4().simple());
        let inner = Arc::new(Mutex::new(TaskInner {
            status: BackgroundStatus::Running,
            output: TaskOutputBuffer::default(),
            pending: true,
            shell_output: capture.as_ref().map(|capture| capture.handle.clone()),
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
        let stdout_pipe = child.stdout.take().expect("stdout pipe");
        let stderr_pipe = child.stderr.take().expect("stderr pipe");
        let sink = self.sink.clone();
        let monitor_id = id.clone();
        let monitor = tokio::spawn(async move {
            // 读取 future 随 monitor 取消回收；正常退出时等管道读尽再落终态。
            let handle = capture.as_ref().map(|capture| &capture.handle);
            let (exit, stdout, stderr) = tokio::join!(
                child.wait(),
                super::shell_output::read_output(stdout_pipe, handle, None, |chunk| {
                    inner.lock().expect("task state lock").output.push(chunk);
                }),
                super::shell_output::read_output(stderr_pipe, handle, None, |chunk| {
                    inner.lock().expect("task state lock").output.push(chunk);
                }),
            );
            let read_error = stdout.err().or_else(|| stderr.err());
            if let Some(handle) = handle {
                handle.finish(read_error.clone().or_else(|| {
                    exit.as_ref()
                        .err()
                        .map(|e| format!("Failed to wait for command: {e}"))
                }));
            }
            drop(capture);
            // TaskStop / 会话回收已置 Killed 且结果已内联或会话已退出：不再回投
            let notify = {
                let mut inner = inner.lock().expect("task state lock poisoned");
                if inner.status == BackgroundStatus::Killed {
                    false
                } else {
                    inner.status = match (read_error, exit) {
                        (Some(error), _) => BackgroundStatus::Failed { error },
                        (_, Err(error)) => BackgroundStatus::Failed {
                            error: format!("Failed to wait for command: {error}"),
                        },
                        (_, Ok(status)) => BackgroundStatus::Exited {
                            code: status.code(),
                        },
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
            tail: truncate_utf8(
                &inner
                    .output
                    .data
                    .iter()
                    .skip(inner.output.data.len().saturating_sub(TASK_OUTPUT_TAIL + 3))
                    .copied()
                    .collect::<Vec<_>>(),
                TASK_OUTPUT_TAIL,
                TruncateSide::Tail,
            ),
            shell_output: inner.shell_output.as_ref().map(OutputHandle::snapshot),
        })
    }

    /// 运行中或完成结果尚未交给会话的任务数。
    pub fn pending_count(&self) -> usize {
        self.tasks
            .lock()
            .expect("background tasks lock poisoned")
            .values()
            .filter(|task| task.inner.lock().expect("task state lock poisoned").pending)
            .count()
    }

    pub fn acknowledge(&self, id: &str) {
        if let Some(task) = self
            .tasks
            .lock()
            .expect("background tasks lock poisoned")
            .get(id)
        {
            task.inner.lock().expect("task state lock poisoned").pending = false;
        }
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
        if let Some(output) = &inner.shell_output {
            output.finish(Some("Command stopped".into()));
        }
        inner.pending = false;
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
        "Check the status and captured output of a background task started with Shell run_in_background. Returns the status, total bytes captured, and the most recent output; completion automatically resumes the session, so use this tool only when you need to inspect progress or read more output."
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
        self.run(arguments).map(|(text, _)| text)
    }

    async fn execute_result(&self, call: &crate::model::ToolCall) -> crate::model::ToolResult {
        match self.run(&call.arguments) {
            Ok((text, task)) => crate::model::ToolResult {
                task: Some(task),
                ..crate::model::ToolResult::success(&call.id, text)
            },
            Err(error) => crate::model::ToolResult::error(&call.id, error),
        }
    }
}

impl TaskOutputTool {
    fn run(&self, arguments: &str) -> Result<(String, crate::model::BackgroundTask), String> {
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
        Ok((text, snapshot.task()))
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
             task_id. Shell stops return their status here; pending session replies report the \
             interruption automatically."
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
        self.run(arguments).await.map(|(text, _)| text)
    }

    async fn execute_result(&self, call: &crate::model::ToolCall) -> crate::model::ToolResult {
        match self.run(&call.arguments).await {
            Ok((text, task)) => crate::model::ToolResult {
                task: Some(task),
                ..crate::model::ToolResult::success(&call.id, text)
            },
            Err(error) => crate::model::ToolResult::error(&call.id, error),
        }
    }
}

impl TaskStopTool {
    async fn run(&self, arguments: &str) -> Result<(String, crate::model::BackgroundTask), String> {
        let args: TaskIdArgs = parse_args(arguments, "TaskStop")?;
        if args.task_id.starts_with("sess_") {
            let broker = self
                .broker
                .as_ref()
                .ok_or_else(|| "Session control is not available in this context".to_string())?;
            broker.abort_session(&args.task_id).await?;
            let mut task = crate::model::BackgroundTask::running(args.task_id.clone());
            task.status = crate::model::BackgroundTaskStatus::Stopped;
            return Ok((format!("Session {} turn aborted.", args.task_id), task));
        }
        let text = self.tasks.stop(&args.task_id)?;
        let task = self
            .tasks
            .snapshot(&args.task_id)
            .ok_or_else(|| format!("Background task {} {NOT_FOUND}", args.task_id))?;
        Ok((text, task.task()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio::io::AsyncReadExt;

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
        assert_eq!(
            buffer
                .data
                .iter()
                .skip(BACKGROUND_BUFFER_CAP - 10)
                .copied()
                .collect::<Vec<_>>(),
            b"tail-bytes"
        );
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
                shell_output: None,
                pending: true,
                status: BackgroundStatus::Running,
                output: TaskOutputBuffer::default(),
            }));
            let (stdout, mut stdout_peer) = tokio::io::duplex(64);
            let (stderr, mut stderr_peer) = tokio::io::duplex(64);
            let stdout_inner = inner.clone();
            let stderr_inner = inner.clone();
            let stdout =
                super::super::shell_output::read_output(stdout, None, None, move |chunk| {
                    stdout_inner.lock().unwrap().output.push(chunk);
                });
            let stderr =
                super::super::shell_output::read_output(stderr, None, None, move |chunk| {
                    stderr_inner.lock().unwrap().output.push(chunk);
                });
            let monitor = tokio::spawn(async move {
                let _ = tokio::join!(stdout, stderr);
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
