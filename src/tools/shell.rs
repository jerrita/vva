//! Shell 工具的执行骨架：带超时与退出码检查。

use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use super::Tool;
use super::parse_args;

/// 单条命令的执行超时
const SHELL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ShellTool {
    working_dir: PathBuf,
    background: Option<Arc<super::background::BackgroundTasks>>,
}

impl ShellTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            background: None,
        }
    }

    /// 绑定会话级后台任务注册表，启用 run_in_background
    pub fn with_background(mut self, tasks: Arc<super::background::BackgroundTasks>) -> Self {
        self.background = Some(tasks);
        self
    }
}

#[derive(Deserialize)]
struct ShellArgs {
    intent: String,
    command: String,
    #[serde(default)]
    run_in_background: bool,
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "Shell"
    }

    fn description(&self) -> &str {
        shell_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "intent": { "type": "string", "description": "Required nonempty, brief explanation of what the command will accomplish, shown in the UI." },
                "command": { "type": "string", "description": shell_command_hint() },
                "run_in_background": { "type": "boolean", "description": "Run detached with no timeout: returns a task id immediately while the command keeps running. Check status and output with TaskOutput, stop with TaskStop. You are notified when the task exits. Tasks die when the session ends; in one-shot CLI runs that is process exit." }
            },
            "required": ["intent", "command"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        self.run(arguments).await.map(|(text, _)| text)
    }

    async fn execute_result(&self, call: &crate::model::ToolCall) -> crate::model::ToolResult {
        super::result_with_task(call, self.run(&call.arguments).await)
    }
}

impl ShellTool {
    async fn run(&self, arguments: &str) -> Result<(String, Option<String>), String> {
        let args: ShellArgs = parse_args(arguments, "Shell")?;
        if args.intent.trim().is_empty() {
            return Err("intent must not be empty".to_string());
        }

        if args.run_in_background {
            let tasks = self.background.as_ref().ok_or_else(|| {
                "Background execution is not available in this context".to_string()
            })?;
            let id = tasks.spawn(&args.intent, &args.command, &self.working_dir)?;
            return Ok((
                format!(
                    "Background task started: {id}\nintent: {}\ncommand: {}\nNo timeout applies. Use TaskOutput with task_id \"{id}\" to check status and output; TaskStop to stop it. You will be notified when it exits.",
                    args.intent, args.command
                ),
                Some(id),
            ));
        }

        let mut command = build_shell_command(&args.command);
        command.current_dir(&self.working_dir);
        run_shell_command(command).await.map(|text| (text, None))
    }
}

/// 组装平台相关的 shell 命令（bash -c / PowerShell 包装），前台与后台执行共用
pub(crate) fn build_shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    let cmd = {
        let wrapped = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Console]::InputEncoding = [System.Text.Encoding]::UTF8; {}",
            command
        );
        let mut cmd = tokio::process::Command::new("powershell");
        cmd.arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(&wrapped);
        cmd
    };
    #[cfg(not(windows))]
    let cmd = {
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c").arg(command);
        cmd
    };
    cmd
}

/// 平台相关的工具描述与参数提示：一处条件编译选定平台名词，其余文本统一拼装。
#[cfg(windows)]
const SHELL_NOUN: &str = "PowerShell";
#[cfg(not(windows))]
const SHELL_NOUN: &str = "bash";

#[cfg(windows)]
const SHELL_INVOCATION: &str = "non-interactive PowerShell (-NoProfile -NonInteractive)";
#[cfg(not(windows))]
const SHELL_INVOCATION: &str = "bash -c";

fn shell_description() -> &'static str {
    static SHELL_DESCRIPTION: LazyLock<String> = LazyLock::new(|| {
        [
            "Run a command for builds, tests, version control, or other terminal operations. Prefer Read, Write, Edit, Glob, and Grep for their supported file operations. Executes with ",
            SHELL_INVOCATION,
            ", starting at the workspace root each time; shell state does not persist between calls.\nProvide a nonempty intent describing the purpose. Use ",
            SHELL_NOUN,
            " syntax and shell-quote paths containing spaces within command. Commands requiring interactive input are unsuitable. The timeout is 120 seconds. Returns stdout and a separate STDERR section when present; nonzero exit status and timeout produce errors. Output larger than 32 KiB is saved to the project tool_outputs directory; the result includes the first 32 KiB followed by a size notice and that path so you can Read or Grep the full output.\nFor long-running commands (dev servers, watchers, builds), set run_in_background to true: the command runs detached with no timeout, the call returns a task id immediately, stdout and stderr are captured merged, and you are notified when it exits. Check progress with TaskOutput, stop early with TaskStop. Background tasks die when the session ends; in one-shot CLI runs that is process exit.\nExample arguments: {\"intent\":\"Check working tree changes\",\"command\":\"git status --short\"}.",
        ]
        .concat()
    });
    SHELL_DESCRIPTION.as_str()
}

fn shell_command_hint() -> String {
    format!(
        "Shell command executed from the workspace root. Use {SHELL_NOUN} syntax. Shell-quote paths with spaces and JSON-escape those quotes in this string."
    )
}

/// 执行已组装好的 shell 命令：stdout/stderr 拼接（STDERR 分节），非零退出码报错。
/// 超时后强制回收子进程（kill_on_drop）。
async fn run_shell_command(mut cmd: tokio::process::Command) -> Result<String, String> {
    cmd.kill_on_drop(true);

    let output = tokio::time::timeout(SHELL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            format!(
                "Command timed out after {}s and was killed",
                SHELL_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("Failed to run command: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();
    result.push_str(&stdout);
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("STDERR:\n");
        result.push_str(&stderr);
    }

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        return Err(format!("Command exited with status code {code}:\n{result}"));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_shell_command_stdout_and_stderr() {
        let cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("powershell");
            c.arg("-Command")
                .arg("Write-Output ok; [Console]::Error.WriteLine('err')");
            c
        } else {
            let mut c = tokio::process::Command::new("bash");
            c.arg("-c").arg("echo ok; echo err >&2");
            c
        };
        let out = run_shell_command(cmd).await.expect("run");
        assert!(out.contains("ok"));
        assert!(out.contains("STDERR:"));
        assert!(out.contains("err"));
    }

    #[tokio::test]
    async fn test_run_shell_command_nonzero_exit() {
        let cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("powershell");
            c.arg("-Command").arg("exit 42");
            c
        } else {
            let mut c = tokio::process::Command::new("bash");
            c.arg("-c").arg("exit 42");
            c
        };
        let err = run_shell_command(cmd).await.unwrap_err();
        assert!(err.contains("42"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn background_spawn_returns_immediately_and_stays_running() {
        let tasks = Arc::new(super::super::background::BackgroundTasks::new(Arc::new(
            |_| {},
        )));
        let registry = crate::tools::ToolRegistry::with_workspace_tools(
            &std::env::temp_dir(),
            None,
            crate::model::EditorKind::Replace,
            crate::tools::WorkspaceToolDeps {
                background: Some(tasks.clone()),
                ..Default::default()
            },
        );
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 2"
        } else {
            "sleep 2"
        };

        let started = std::time::Instant::now();
        let result = registry
            .execute_call(&crate::model::ToolCall::new(
                "call-background",
                "Shell",
                serde_json::json!({
                    "intent": "watch build", "command": command, "run_in_background": true
                })
                .to_string(),
            ))
            .await;
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("Background task started: "));
        let id = result.background_task_id.expect("background task identity");
        assert_eq!(
            tasks.snapshot(&id).expect("task registered").status,
            super::super::background::BackgroundStatus::Running
        );
        tasks.stop(&id).unwrap();
    }

    #[tokio::test]
    async fn background_without_registry_errors() {
        let tool = ShellTool::new(std::env::temp_dir());
        let error = tool
            .execute(r#"{"intent":"x","command":"echo hi","run_in_background":true}"#)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "Background execution is not available in this context"
        );
    }

    #[test]
    fn parameters_declare_optional_run_in_background() {
        let schema = ShellTool::new(std::env::temp_dir()).parameters();
        let property = &schema["properties"]["run_in_background"];
        assert_eq!(property["type"], "boolean");
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|key| key == "run_in_background"));
    }
}
