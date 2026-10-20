//! Write 工具：独占创建写。

use super::{Tool, resolve_path, write_new_file};
use crate::tools::{names, parse_args};
use async_trait::async_trait;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

pub struct WriteTool {
    working_dir: PathBuf,
}

impl WriteTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct WriteArgs {
    pub(super) path: String,
    pub(super) content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        names::WRITE
    }

    fn description(&self) -> &str {
        r#"Create a new UTF-8 text file with complete contents. Use Edit to change an existing file. Missing parent directories are created recursively; fails if the target exists. Returns the created file path.
Paths are absolute or workspace-relative; shell variables and ~ are not expanded. Example arguments: {"path":"src/example.rs","content":"pub fn example() {}\n"}."#
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "format": "path", "description": "New file path, absolute or workspace-relative, missing parent directories are created." },
                    "content": { "type": "string", "description": "Complete literal file contents. Use JSON escapes for newlines and quotes; do not include Markdown fences unless they belong in the file." }
                },
                "required": ["path", "content"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: WriteArgs = parse_args(arguments, names::WRITE)?;
        let path = resolve_path(&self.working_dir, &args.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("Failed to create directory {}: {error}", parent.display())
            })?;
        }
        write_new_file(&path, &args.content)?;
        Ok(format!("Created file {}", path.display()))
    }
}
