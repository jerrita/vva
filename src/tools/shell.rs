//! Shell 工具的执行骨架：带超时与退出码检查。

use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

use super::Tool;

/// 单条命令的执行超时
const SHELL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ShellTool {
    working_dir: PathBuf,
}

impl ShellTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
struct ShellArgs {
    intent: String,
    command: String,
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
                "command": { "type": "string", "description": shell_command_hint() }
            },
            "required": ["intent", "command"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: ShellArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Shell: {error}"))?;
        if args.intent.trim().is_empty() {
            return Err("intent must not be empty".to_string());
        }

        #[cfg(windows)]
        let mut command = {
            let wrapped = format!(
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Console]::InputEncoding = [System.Text.Encoding]::UTF8; {}",
                args.command
            );
            let mut command = tokio::process::Command::new("powershell");
            command
                .arg("-NoProfile")
                .arg("-NonInteractive")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-Command")
                .arg(&wrapped);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = tokio::process::Command::new("bash");
            command.arg("-c").arg(&args.command);
            command
        };
        command.current_dir(&self.working_dir);
        run_shell_command(command).await
    }
}

/// 平台相关的工具描述与参数提示：条件编译区分 bash 与 PowerShell。
#[cfg(windows)]
fn shell_description() -> &'static str {
    r#"Run a command for builds, tests, version control, or other terminal operations. Prefer Read, Write, Edit, Glob, and Grep for their supported file operations. Executes with non-interactive PowerShell (-NoProfile -NonInteractive), starting at the workspace root each time; shell state does not persist between calls.
Provide a nonempty intent describing the purpose. Use PowerShell syntax and shell-quote paths containing spaces within command. Commands requiring interactive input are unsuitable. The timeout is 120 seconds. Returns stdout and a separate STDERR section when present; nonzero exit status and timeout produce errors. Output larger than 32 KiB is saved to the project tool_outputs directory; the result then contains that path so you can Read or Grep it.
Example arguments: {"intent":"Check working tree changes","command":"git status --short"}."#
}

#[cfg(not(windows))]
fn shell_description() -> &'static str {
    r#"Run a command for builds, tests, version control, or other terminal operations. Prefer Read, Write, Edit, Glob, and Grep for their supported file operations. Executes with bash -c, starting at the workspace root each time; shell state does not persist between calls.
Provide a nonempty intent describing the purpose. Use bash syntax and shell-quote paths containing spaces within command. Commands requiring interactive input are unsuitable. The timeout is 120 seconds. Returns stdout and a separate STDERR section when present; nonzero exit status and timeout produce errors. Output larger than 32 KiB is saved to the project tool_outputs directory; the result then contains that path so you can Read or Grep it.
Example arguments: {"intent":"Check working tree changes","command":"git status --short"}."#
}

#[cfg(windows)]
fn shell_command_hint() -> &'static str {
    "Shell command executed from the workspace root. Use PowerShell syntax. Shell-quote paths with spaces and JSON-escape those quotes in this string."
}

#[cfg(not(windows))]
fn shell_command_hint() -> &'static str {
    "Shell command executed from the workspace root. Use bash syntax. Shell-quote paths with spaces and JSON-escape those quotes in this string."
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
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
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
}
