use std::path::PathBuf;
use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use super::Tool;

pub struct BashTool {
    working_dir: PathBuf,
}

impl BashTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command inside the workspace directory."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: BashArgs = serde_json::from_str(arguments)
            .map_err(|e| format!("Invalid arguments for bash tool: {e}"))?;

        let output = Command::new("bash")
            .arg("-c")
            .arg(&args.command)
            .current_dir(&self.working_dir)
            .output()
            .await
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
}

