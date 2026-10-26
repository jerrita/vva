//! Shell 工具的执行骨架：带超时与退出码检查。

use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::Tool;
use super::names;
use super::parse_args;

/// 单条命令的执行超时
const SHELL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ShellTool {
    working_dir: PathBuf,
    background: Option<Arc<super::background::BackgroundTasks>>,
    description: String,
    outputs: Option<Arc<super::shell_output::ShellOutputs>>,
    output_dir: Option<PathBuf>,
}

impl ShellTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            background: None,
            outputs: None,
            output_dir: None,
            description: shell_description(&[
                names::READ,
                names::WRITE,
                names::EDIT,
                names::GLOB,
                names::GREP,
            ]),
        }
    }

    pub(crate) fn with_outputs(
        mut self,
        outputs: Option<Arc<super::shell_output::ShellOutputs>>,
        output_dir: Option<PathBuf>,
    ) -> Self {
        self.outputs = outputs;
        self.output_dir = output_dir;
        self
    }

    /// 按当前编辑器实际注册的文件工具名生成描述
    pub fn with_file_tools(mut self, file_tools: &[&str]) -> Self {
        self.description = shell_description(file_tools);
        self
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
        names::SHELL
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            serde_json::json!({
                "type": "object",
                "properties": {
                    "intent": { "type": "string", "description": "Brief, nonempty explanation of what the command accomplishes, shown in the UI." },
                    "command": { "type": "string", "description": shell_command_hint() },
                    "run_in_background": { "type": "boolean", "description": "Run detached with no timeout: returns a task id immediately while the command keeps running. Check status and output with TaskOutput, stop with TaskStop. Completion automatically resumes this session, even after you end your response. Continue other work or end your response; do not sleep or poll for completion. Tasks die when the session ends; in one-shot CLI runs that is process exit." }
                },
                "required": ["intent", "command"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        self.run(arguments, None).await.map(|(text, _)| text)
    }

    async fn execute_result(&self, call: &crate::model::ToolCall) -> crate::model::ToolResult {
        let capture = self
            .outputs
            .as_ref()
            .map(|outputs| outputs.capture(&call.id));
        let handle = capture.as_ref().map(|capture| capture.handle.clone());
        let outcome = self.run(&call.arguments, capture).await;
        let mut result = super::result_with_task(call, outcome);
        if result.background_task_id.is_none() {
            result.shell_output = handle.map(|handle| Box::new(handle.snapshot()));
        }
        if let Some(id) = &result.background_task_id {
            result.task = self
                .background
                .as_ref()
                .and_then(|tasks| tasks.snapshot(id))
                .map(|snapshot| snapshot.task());
        }
        result
    }
}

impl ShellTool {
    async fn run(
        &self,
        arguments: &str,
        mut capture: Option<super::shell_output::OutputCapture>,
    ) -> Result<(String, Option<String>), String> {
        let result = self.run_captured(arguments, &mut capture).await;
        if let Some(capture) = &capture {
            capture.handle.finish(
                result
                    .as_ref()
                    .err()
                    .map(|error| error.lines().next().unwrap_or(error).to_string()),
            );
        }
        result
    }

    async fn run_captured(
        &self,
        arguments: &str,
        capture: &mut Option<super::shell_output::OutputCapture>,
    ) -> Result<(String, Option<String>), String> {
        let args: ShellArgs = parse_args(arguments, names::SHELL)?;
        if args.intent.trim().is_empty() {
            return Err("intent must not be empty".to_string());
        }

        if args.run_in_background {
            let tasks = self.background.as_ref().ok_or_else(|| {
                "Background execution is not available in this context".to_string()
            })?;
            let id = tasks.spawn_captured(
                &args.intent,
                &args.command,
                &self.working_dir,
                capture.take(),
            )?;
            return Ok((
                format!(
                    "Background task started: {id}\nintent: {}\ncommand: {}\nNo timeout applies. Use TaskOutput with task_id \"{id}\" to check status and output; TaskStop to stop it. Completion automatically resumes this session, even after you end your response. Continue other work or end your response; do not sleep or poll for completion.",
                    args.intent, args.command
                ),
                Some(id),
            ));
        }

        let command = workspace_shell_command(&args.command, &self.working_dir)?;
        run_shell_command(
            command,
            capture.as_ref().map(|capture| &capture.handle),
            self.output_dir.as_deref(),
        )
        .await
        .map(|text| (text, None))
    }
}

/// 在工作区内组装 shell 命令：设置工作目录，并注入本次读取的项目 env（`.vva/env`）
pub(crate) fn workspace_shell_command(
    command: &str,
    working_dir: &Path,
) -> Result<tokio::process::Command, String> {
    let env = crate::config::env::workspace_env(working_dir)?;
    let mut cmd = build_shell_command(command);
    cmd.current_dir(working_dir).envs(env);
    Ok(cmd)
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

fn shell_description(file_tools: &[&str]) -> String {
    let (init, last) = file_tools.split_at(file_tools.len().saturating_sub(1));
    let listed = match (init, last) {
        ([], [only]) => (*only).to_string(),
        (init, [last]) => format!("{}, and {last}", init.join(", ")),
        _ => String::new(),
    };
    let browse: Vec<&str> = file_tools
        .iter()
        .copied()
        .filter(|name| {
            [
                names::READ,
                names::GREP,
                names::HASHLINE_READ,
                names::HASHLINE_GREP,
            ]
            .contains(name)
        })
        .collect();
    [
        "Run a command for builds, tests, version control, or other terminal operations. Use ",
        &listed,
        " for file operations. Executes with ",
        SHELL_INVOCATION,
        ", starting at the workspace root each time; shell state does not persist between calls.\nProvide a nonempty intent describing the purpose. Use ",
        SHELL_NOUN,
        " syntax and shell-quote paths containing spaces within command. Commands run non-interactively with a 120-second timeout. Returns stdout and a separate STDERR section when present; nonzero exit status and timeout produce errors. Output larger than 32 KiB is saved to the project tool_outputs directory; the result includes the first 32 KiB followed by a size notice and that path so you can ",
        &browse.join(" or "),
        " the full output.\nFor long-running commands (dev servers, watchers, builds), set run_in_background to true: the command runs detached with no timeout, the call returns a task id immediately, stdout and stderr are captured merged, and completion automatically resumes this session, even after you end your response. Continue other work or end your response; do not sleep or poll for completion. Check progress with TaskOutput, stop early with TaskStop. Background tasks die when the session ends; in one-shot CLI runs that is process exit.\nExample arguments: {\"intent\":\"Check working tree changes\",\"command\":\"git status --short\"}.",
    ]
    .concat()
}

fn shell_command_hint() -> String {
    let escaping = if cfg!(windows) {
        ""
    } else {
        " After JSON decoding, command is passed directly to bash without extra unescaping. In bash double quotes, literal \\n stays literal; use actual newlines or ANSI-C quoting ($'first\\nsecond') when an argument needs a newline."
    };
    format!(
        "Shell command executed from the workspace root. Use {SHELL_NOUN} syntax. Shell-quote paths with spaces and JSON-escape those quotes in this string.{escaping}"
    )
}

/// 输出逐块写入匿名临时文件，内存只保留有界展示尾部和模型回执头部。
async fn run_shell_command(
    mut cmd: tokio::process::Command,
    capture: Option<&super::shell_output::OutputHandle>,
    output_dir: Option<&Path>,
) -> Result<String, String> {
    use std::process::Stdio;
    let file = || {
        tempfile::tempfile()
            .map(tokio::fs::File::from_std)
            .map_err(|e| format!("Failed to create command output file: {e}"))
    };
    let mut stdout_file = file()?;
    let mut stderr_file = file()?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run command: {e}"))?;
    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let outcome = tokio::time::timeout(SHELL_TIMEOUT, async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            super::shell_output::read_output(stdout, capture, Some(&mut stdout_file), |_| {}),
            super::shell_output::read_output(stderr, capture, Some(&mut stderr_file), |_| {}),
        );
        stdout?;
        stderr?;
        status.map_err(|e| format!("Failed to wait for command: {e}"))
    })
    .await;
    let status = match outcome {
        Ok(status) => status,
        Err(_) => {
            let _ = child.kill().await;
            Err(format!(
                "Command timed out after {}s and was killed",
                SHELL_TIMEOUT.as_secs()
            ))
        }
    };
    let output = format_output(stdout_file, stderr_file, output_dir).await?;
    let result = match status {
        Ok(status) if status.success() => Ok(output),
        Ok(status) => Err(format!(
            "Command exited with status code {}:\n{output}",
            status.code().unwrap_or(-1)
        )),
        Err(error) => Err(format!("{error}\n{output}")),
    };
    if let Some(capture) = capture {
        capture.finish(
            result
                .as_ref()
                .err()
                .map(|error| error.lines().next().unwrap_or(error).to_string()),
        );
    }
    result
}

async fn format_output(
    mut stdout: tokio::fs::File,
    mut stderr: tokio::fs::File,
    output_dir: Option<&Path>,
) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
    let result = async {
        // seek 等待之前异步 write 的阻塞任务完成。
        let stdout_len = stdout.seek(std::io::SeekFrom::End(0)).await?;
        let stderr_len = stderr.seek(std::io::SeekFrom::End(0)).await?;
        stdout.seek(std::io::SeekFrom::Start(0)).await?;
        stderr.seek(std::io::SeekFrom::Start(0)).await?;
        let separator = if stderr_len == 0 { "" } else if stdout_len == 0 { "STDERR:\n" } else { "\nSTDERR:\n" };
        let total = stdout_len + stderr_len + separator.len() as u64;
        let limit = super::INLINE_OUTPUT_BUDGET;
        if total <= limit as u64 {
            let mut bytes = Vec::with_capacity(total as usize);
            stdout.read_to_end(&mut bytes).await?;
            bytes.extend_from_slice(separator.as_bytes());
            stderr.read_to_end(&mut bytes).await?;
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        let mut head = Vec::with_capacity(limit);
        (&mut stdout).take(limit as u64).read_to_end(&mut head).await?;
        if head.len() < limit {
            head.extend_from_slice(separator.as_bytes());
            (&mut stderr).take(limit.saturating_sub(head.len()) as u64).read_to_end(&mut head).await?;
        }
        let head = super::truncate_utf8(&head, limit, super::TruncateSide::Head);
        let notice = if let Some(dir) = output_dir {
            tokio::fs::create_dir_all(dir).await?;
            let path = dir.join(format!("Shell_{}.txt", uuid::Uuid::new_v4().simple()));
            let mut file = tokio::fs::File::create(&path).await?;
            stdout.seek(std::io::SeekFrom::Start(0)).await?;
            stderr.seek(std::io::SeekFrom::Start(0)).await?;
            tokio::io::copy(&mut stdout, &mut file).await?;
            file.write_all(separator.as_bytes()).await?;
            tokio::io::copy(&mut stderr, &mut file).await?;
            file.flush().await?;
            format!("Output too large ({total} bytes). Full output saved to {}. Use the file-reading tools on that path.", path.display())
        } else {
            format!("Output truncated ({total} bytes; showing the first {limit} bytes).")
        };
        let mut inline_len = head.len().min(super::INLINE_OUTPUT_BUDGET.saturating_sub(notice.len() + 2));
        while !head.is_char_boundary(inline_len) { inline_len -= 1; }
        Ok::<_, std::io::Error>(format!("{}\n\n{notice}", &head[..inline_len]))
    }.await;
    result.map_err(|e| format!("Failed to save command output: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn foreground_and_background_stream_before_exit_and_persist_final_tail() {
        use super::super::shell_output::ShellOutputs;
        use crate::model::ToolCall;
        for background in [false, true] {
            let workspace = tempfile::tempdir().unwrap();
            let outputs = Arc::new(ShellOutputs::default());
            let tasks = Arc::new(super::super::background::BackgroundTasks::new(Arc::new(
                |_| {},
            )));
            let tool = ShellTool::new(workspace.path())
                .with_background(tasks.clone())
                .with_outputs(
                    Some(outputs.clone()),
                    Some(workspace.path().join("outputs")),
                );
            let call = ToolCall::new("synthetic", names::SHELL, serde_json::json!({
                "intent": "Stream fixture", "command": "printf ready; while [ ! -f release ]; do sleep 0.01; done; printf complete",
                "run_in_background": background,
            }).to_string());
            let work = tokio::spawn(async move { tool.execute_result(&call).await });
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    outputs.notified().await;
                    if let Some((_, output)) = outputs
                        .drain()
                        .into_iter()
                        .find(|(_, output)| output.text == "ready")
                    {
                        assert!(!output.finished);
                        break;
                    }
                }
            })
            .await
            .unwrap();
            if !background {
                assert!(!work.is_finished());
            }
            tokio::fs::write(workspace.path().join("release"), "")
                .await
                .unwrap();
            let result = work.await.unwrap();
            assert!(!result.is_error, "{}", result.content);
            let final_output = if let Some(id) = result.background_task_id {
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let snapshot = tasks.snapshot(&id).unwrap();
                        if snapshot.status != super::super::background::BackgroundStatus::Running {
                            break snapshot.task();
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap()
                .shell_output
                .unwrap()
            } else {
                result.shell_output.unwrap()
            };
            assert!(final_output.finished);
            assert_eq!(final_output.text, "readycomplete");
            assert!(final_output.error.is_none());
        }
    }

    #[tokio::test]
    async fn large_output_spools_complete_stdout_and_stderr_with_bounded_receipt() {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().unwrap();
        let mut stdout = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        let mut stderr = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        stdout.write_all(&vec![b'x'; 512 * 1024]).await.unwrap();
        stderr.write_all(b"diagnostic").await.unwrap();
        let receipt = format_output(stdout, stderr, Some(dir.path()))
            .await
            .unwrap();
        assert!(receipt.len() <= super::super::MAX_INLINE_OUTPUT_BYTES);
        let path = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let full = std::fs::read(path).unwrap();
        assert_eq!(full.len(), 512 * 1024 + "\nSTDERR:\ndiagnostic".len());
        assert!(full.ends_with(b"\nSTDERR:\ndiagnostic"));
    }

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
        let out = run_shell_command(cmd, None, None).await.expect("run");
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
        let err = run_shell_command(cmd, None, None).await.unwrap_err();
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
                names::SHELL,
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
        let schema = ShellTool::new(std::env::temp_dir()).input().expect_json();
        let property = &schema["properties"]["run_in_background"];
        assert_eq!(property["type"], "boolean");
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|key| key == "run_in_background"));
    }
}
