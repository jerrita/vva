use async_trait::async_trait;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use super::Tool;

pub struct FileViewTool {
    working_dir: PathBuf,
}

impl FileViewTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    fn resolve_path(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.working_dir.join(p)
        }
    }
}

#[derive(Deserialize)]
struct FileViewArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[async_trait]
impl Tool for FileViewTool {
    fn name(&self) -> &str {
        "file_view"
    }

    fn description(&self) -> &str {
        "View the contents of a file with optional start_line and end_line parameters."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to view"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Optional 1-indexed starting line number"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Optional 1-indexed ending line number"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: FileViewArgs = serde_json::from_str(arguments)
            .map_err(|e| format!("Invalid arguments for file_view: {e}"))?;

        let path = self.resolve_path(&args.path);
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read file {}: {e}", path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let start = args.start_line.unwrap_or(1).saturating_sub(1);
        let end = args.end_line.unwrap_or(lines.len()).min(lines.len());

        if start >= lines.len() {
            return Ok(String::new());
        }

        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{:4}: {}\n", start + i + 1, line));
        }

        Ok(out)
    }
}

pub struct FileEditTool {
    working_dir: PathBuf,
}

impl FileEditTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    fn resolve_path(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.working_dir.join(p)
        }
    }
}

#[derive(Deserialize)]
struct FileEditArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write full content to a file, creating parent directories if needed."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "New content to write"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: FileEditArgs = serde_json::from_str(arguments)
            .map_err(|e| format!("Invalid arguments for file_write: {e}"))?;

        let path = self.resolve_path(&args.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent directories: {e}"))?;
        }

        fs::write(&path, &args.content)
            .map_err(|e| format!("Failed to write file {}: {e}", path.display()))?;

        Ok(format!(
            "Successfully wrote {} bytes to {}",
            args.content.len(),
            path.display()
        ))
    }
}
