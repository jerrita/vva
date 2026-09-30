//! Glob 工具：模式文件枚举。

use super::{
    MAX_RESULTS, Tool, collect_files, matches_any, parse_glob_patterns, relative_to, resolve_path,
    sort_by_mtime,
};
use crate::tools::parse_args;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;

pub struct GlobTool {
    working_dir: PathBuf,
}

impl GlobTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct GlobArgs {
    pub(super) pattern: String,
    pub(super) path: Option<String>,
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        r#"Find files by path pattern; use Grep to search file contents. Matches are case-sensitive and relative to the search root. Supports *, ?, character classes such as [ab], ** as a full path component for recursive matching, and brace expansion such as **/*.{rs,md,toml}. A single * does not cross directory separators. Extglobs are unsupported.
Returns file paths sorted by modification time (newest first), at most 100, with a truncation marker when more match. Empty output means no matches. Hidden and gitignored files are included; directory symlinks are not traversed during recursion. Example arguments: {"pattern":"**/*.{rs,md}","path":"src"}. Shell variables and ~ are not expanded."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Case-sensitive glob relative to path, e.g. **/*.rs, **/*.{rs,md,toml}, or Cargo.toml. Supports *, ?, [...], recursive **, and {a,b} expansion. Extglobs are unsupported." },
                "path": { "type": "string", "format": "path", "description": "Search root: a directory or a single file, absolute or workspace-relative. Omit for the workspace root. Shell variables and ~ are not expanded." }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: GlobArgs = parse_args(arguments, "Glob")?;
        let root = resolve_path(&self.working_dir, args.path.as_deref().unwrap_or("."));
        let patterns = parse_glob_patterns(&args.pattern)
            .map_err(|error| format!("Invalid glob pattern: {error}"))?;
        let root_is_file = root.is_file();
        let mut files = Vec::new();
        collect_files(&root, &mut files)?;
        let mut matches: Vec<_> = files
            .into_iter()
            .filter(|path| matches_any(&patterns, relative_to(&root, path, root_is_file)))
            .collect();
        sort_by_mtime(&mut matches)?;
        matches.dedup();

        let truncated = matches.len() > MAX_RESULTS;
        matches.truncate(MAX_RESULTS);
        let mut output = matches
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            output.push_str("\n[results truncated]");
        }
        Ok(output)
    }
}
