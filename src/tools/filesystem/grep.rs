//! Grep 工具：正则行搜索。

use super::{
    MAX_RESULTS, Tool, collect_files, matches_any, parse_glob_patterns, relative_to, resolve_path,
    sort_by_mtime,
};
use crate::model::EditorKind;
use crate::tools::parse_args;
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

pub struct GrepTool {
    working_dir: PathBuf,
    editor: EditorKind,
    hashline: Option<std::sync::Arc<crate::tools::hashline::HashlineSession>>,
}

impl GrepTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            editor: EditorKind::Replace,
            hashline: None,
        }
    }

    pub fn with_editor(mut self, editor: EditorKind) -> Self {
        self.editor = editor;
        self
    }

    pub fn with_hashline(
        mut self,
        session: std::sync::Arc<crate::tools::hashline::HashlineSession>,
    ) -> Self {
        self.hashline = Some(session);
        self
    }
}

#[derive(Deserialize)]
pub(super) struct GrepArgs {
    pub(super) pattern: String,
    pub(super) path: Option<String>,
    pub(super) include: Option<String>,
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        match self.editor {
            EditorKind::Hashline => {
                r#"Search UTF-8 file contents line by line with a Rust regular expression; use Glob to find files by name. Matching is case-sensitive by default; use (?i) for case-insensitive search. Supports alternation such as foo|bar, but not lookaround or backreferences. Matches cannot span lines. Non-UTF-8 and unreadable files are skipped.
Optionally restrict files with include, a glob relative to the search root. Glob syntax supports *, ?, character classes, recursive **, and brace expansion such as **/*.{rs,md,toml}. Extglobs are unsupported. Hidden and gitignored files are included; directory symlinks are not traversed during recursion.
Returns [PATH#TAG] then N:text for each match (same header as Read). Copy the header and line numbers into Edit. Sorted by file modification time (newest first), capped at 100 matching lines with a truncation marker. Empty output means no matches. Example arguments: {"pattern":"fn\\s+main|struct","path":"src","include":"**/*.{rs,toml}"}. JSON-escape regex backslashes. Path values do not expand shell variables or ~."#
            }
            EditorKind::Patch | EditorKind::Replace => {
                r#"Search UTF-8 file contents line by line with a Rust regular expression; use Glob to find files by name. Matching is case-sensitive by default; use (?i) for case-insensitive search. Supports alternation such as foo|bar, but not lookaround or backreferences. Matches cannot span lines. Non-UTF-8 and unreadable files are skipped.
Optionally restrict files with include, a glob relative to the search root. Glob syntax supports *, ?, character classes, recursive **, and brace expansion such as **/*.{rs,md,toml}. Extglobs are unsupported. Hidden and gitignored files are included; directory symlinks are not traversed during recursion.
Returns path:line: text with 1-indexed line numbers, sorted by file modification time (newest first), capped at 100 matching lines with a truncation marker. Empty output means no matches. Example arguments: {"pattern":"fn\\s+main|struct","path":"src","include":"**/*.{rs,toml}"}. JSON-escape regex backslashes. Path values do not expand shell variables or ~."#
            }
        }
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regex applied to each line. Use foo|bar for alternatives and (?i) for case-insensitivity. JSON-escape backslashes; lookaround and backreferences are unsupported." },
                "path": { "type": "string", "format": "path", "description": "Search root: a directory or a single file, absolute or workspace-relative. Omit for the workspace root. Shell variables and ~ are not expanded." },
                "include": { "type": "string", "description": "Optional case-sensitive glob relative to path, e.g. **/*.rs or **/*.{rs,md}. Omit to search all files. Supports *, ?, [...], recursive **, and {a,b} expansion. Extglobs are unsupported." }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: GrepArgs = parse_args(arguments, "Grep")?;
        let root = resolve_path(&self.working_dir, args.path.as_deref().unwrap_or("."));
        let regex = Regex::new(&args.pattern)
            .map_err(|error| format!("Invalid regular expression: {error}"))?;
        let include = args
            .include
            .as_deref()
            .map(parse_glob_patterns)
            .transpose()
            .map_err(|error| format!("Invalid include pattern: {error}"))?;
        let root_is_file = root.is_file();
        let mut files = Vec::new();
        collect_files(&root, &mut files)?;
        sort_by_mtime(&mut files)?;

        let mut results = Vec::new();
        let mut match_count = 0usize;
        for path in files {
            let relative = relative_to(&root, &path, root_is_file);
            if include
                .as_ref()
                .is_some_and(|patterns| !matches_any(patterns, relative))
            {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if self.editor == EditorKind::Hashline {
                let mut clipped = Vec::new();
                let mut truncated = false;
                for (index, line) in content.lines().enumerate() {
                    if !regex.is_match(line) {
                        continue;
                    }
                    if match_count == MAX_RESULTS {
                        truncated = true;
                        break;
                    }
                    clipped.push((index, line));
                    match_count += 1;
                }
                if !clipped.is_empty() {
                    let label = path
                        .strip_prefix(&self.working_dir)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                    results.push(crate::tools::hashline::format_grep_file(
                        &label,
                        &content,
                        &clipped,
                        self.hashline.as_deref(),
                        &canonical,
                    )?);
                }
                if truncated {
                    return Ok(format!("{}\n[results truncated]", results.join("\n")));
                }
                continue;
            }
            for (index, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    if match_count == MAX_RESULTS {
                        return Ok(format!("{}\n[results truncated]", results.join("\n")));
                    }
                    results.push(format!("{}:{}: {line}", path.display(), index + 1));
                    match_count += 1;
                }
            }
        }
        Ok(results.join("\n"))
    }
}
