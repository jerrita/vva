//! Shell 类工具的共享执行骨架：带超时执行、输出拼接与长度上限、退出码检查。
//! bash 与 powershell 工具仅声明解释器与命令包装差异，执行语义在此统一。

use std::time::Duration;

/// 单条命令的执行超时
const SHELL_TIMEOUT: Duration = Duration::from_secs(120);

/// 输出字节上限：防止超长输出（如日志转储）直接撑爆模型上下文
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// 执行已组装好的 shell 命令：stdout/stderr 拼接（STDERR 分节），非零退出码报错。
/// 超时后强制回收子进程（kill_on_drop），输出超限按字符边界截断。
pub(crate) async fn run_shell_command(mut cmd: tokio::process::Command) -> Result<String, String> {
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

    if result.len() > MAX_OUTPUT_BYTES {
        let mut cut = MAX_OUTPUT_BYTES;
        while !result.is_char_boundary(cut) {
            cut -= 1;
        }
        result.truncate(cut);
        result.push_str("\n[output truncated]");
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
