use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use tokio::process::Command;

use super::Tool;

pub struct PowerShellTool {
    working_dir: PathBuf,
}

impl PowerShellTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
struct PowerShellArgs {
    command: String,
}

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "powershell"
    }

    fn description(&self) -> &str {
        "Execute a PowerShell command inside the workspace directory."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The PowerShell command line to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: PowerShellArgs = serde_json::from_str(arguments)
            .map_err(|e| format!("Invalid arguments for powershell tool: {e}"))?;

        let wrapped_command = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Console]::InputEncoding = [System.Text.Encoding]::UTF8; {}",
            args.command
        );

        let mut cmd = Command::new("powershell");
        cmd.arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(&wrapped_command)
            .current_dir(&self.working_dir);
        super::shell::run_shell_command(cmd).await
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_powershell_tool_execution() {
        let tool = PowerShellTool::new(std::env::current_dir().unwrap());
        let result = tool
            .execute(r#"{"command":"Write-Output 'hello_powershell'"}"#)
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello_powershell"));
    }

    #[tokio::test]
    async fn test_powershell_tool_error_exit() {
        let tool = PowerShellTool::new(std::env::current_dir().unwrap());
        let result = tool.execute(r#"{"command":"exit 42"}"#).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("42"));
    }
}
