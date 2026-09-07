use async_trait::async_trait;
use glob::{MatchOptions, Pattern};
use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use super::Tool;

const MAX_RESULTS: usize = 1_000;
const GLOB_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

pub fn resolve_path(working_dir: &Path, path: &str) -> PathBuf {
    if path.is_empty() || path == "." {
        return working_dir.to_path_buf();
    }
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    }
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Failed to access {}: {error}", path.display()))?;
    if metadata.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    if metadata.is_dir() {
        visit_files(path, files)?;
    }
    Ok(())
}

fn visit_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(path)
        .map_err(|error| format!("Failed to read directory {}: {error}", path.display()))?
    {
        let entry = entry.map_err(|error| format!("Failed to read directory entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            visit_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn relative_to<'a>(root: &'a Path, path: &'a Path, root_is_file: bool) -> &'a Path {
    if root_is_file {
        path.file_name().map(Path::new).unwrap_or(path)
    } else {
        path.strip_prefix(root).unwrap_or(path)
    }
}

fn matches_glob(pattern: &Pattern, path: &Path) -> bool {
    pattern.matches_path_with(path, GLOB_OPTIONS)
}

fn unified_diff(path: &Path, original: &str, updated: &str) -> String {
    let original_lines: Vec<_> = original.lines().collect();
    let updated_lines: Vec<_> = updated.lines().collect();

    let prefix = original_lines
        .iter()
        .zip(&updated_lines)
        .take_while(|(original, updated)| original == updated)
        .count();
    let suffix = original_lines[prefix..]
        .iter()
        .rev()
        .zip(updated_lines[prefix..].iter().rev())
        .take_while(|(original, updated)| original == updated)
        .count();
    let original_changed_end = original_lines.len() - suffix;
    let updated_changed_end = updated_lines.len() - suffix;
    let context_start = prefix.saturating_sub(3);
    let original_end = (original_changed_end + 3).min(original_lines.len());
    let updated_end = (updated_changed_end + 3).min(updated_lines.len());
    let original_count = original_end - context_start;
    let updated_count = updated_end - context_start;

    let format_range = |start: usize, count: usize| match count {
        0 => format!("{start},0"),
        1 => (start + 1).to_string(),
        _ => format!("{},{}", start + 1, count),
    };

    let mut diff = format!(
        "--- {}\n+++ {}\n@@ -{} +{} @@",
        path.display(),
        path.display(),
        format_range(context_start, original_count),
        format_range(context_start, updated_count),
    );
    for line in &original_lines[context_start..prefix] {
        diff.push_str(&format!("\n {line}"));
    }
    for line in &original_lines[prefix..original_changed_end] {
        diff.push_str(&format!("\n-{line}"));
    }
    for line in &updated_lines[prefix..updated_changed_end] {
        diff.push_str(&format!("\n+{line}"));
    }
    for line in &original_lines[original_changed_end..original_end] {
        diff.push_str(&format!("\n {line}"));
    }
    diff
}

pub struct ReadTool {
    working_dir: PathBuf,
}

impl ReadTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read a file. Lines are returned with 1-indexed line numbers."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "Path to the file" },
                "offset": { "type": "integer", "description": "Optional 1-indexed first line" },
                "limit": { "type": "integer", "description": "Optional maximum number of lines" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: ReadArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Read: {error}"))?;
        let path = resolve_path(&self.working_dir, &args.path);
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("Failed to read file {}: {error}", path.display()))?;
        let lines: Vec<_> = content.lines().collect();
        let start = args.offset.unwrap_or(1).saturating_sub(1);
        let end = args
            .limit
            .map(|limit| start.saturating_add(limit))
            .unwrap_or(lines.len())
            .min(lines.len());

        if start >= lines.len() {
            return Ok(String::new());
        }

        Ok(lines[start..end]
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}: {line}", start + index + 1))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

pub struct EditTool {
    working_dir: PathBuf,
}

impl EditTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Replace exact text in an existing file, optionally replacing every occurrence."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "Path to the file" },
                "old_string": { "type": "string", "description": "Exact text to replace" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace every exact occurrence instead of requiring one match" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: EditArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Edit: {error}"))?;
        if args.old_string.is_empty() {
            return Err("old_string must not be empty".to_string());
        }
        if args.old_string == args.new_string {
            return Err("old_string and new_string must differ".to_string());
        }

        let path = resolve_path(&self.working_dir, &args.path);
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("Failed to read file {}: {error}", path.display()))?;
        let matches = content.match_indices(&args.old_string).count();
        if matches == 0 {
            return Err("old_string was not found".to_string());
        }
        if !args.replace_all.unwrap_or(false) && matches != 1 {
            return Err(format!(
                "old_string must occur exactly once, found {matches}; use replace_all to replace every occurrence"
            ));
        }

        let updated = if args.replace_all.unwrap_or(false) {
            content.replace(&args.old_string, &args.new_string)
        } else {
            content.replacen(&args.old_string, &args.new_string, 1)
        };
        fs::write(&path, &updated)
            .map_err(|error| format!("Failed to write file {}: {error}", path.display()))?;
        Ok(unified_diff(&path, &content, &updated))
    }
}

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
struct GlobArgs {
    pattern: String,
    path: Option<String>,
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "Find workspace files matching a glob pattern."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, including ** for recursive matches" },
                "path": { "type": "string", "format": "path", "description": "Optional directory to search" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: GlobArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Glob: {error}"))?;
        let root = resolve_path(&self.working_dir, args.path.as_deref().unwrap_or("."));
        let pattern = Pattern::new(&args.pattern)
            .map_err(|error| format!("Invalid glob pattern: {error}"))?;
        let root_is_file = root.is_file();
        let mut files = Vec::new();
        collect_files(&root, &mut files)?;
        let mut matches: Vec<_> = files
            .into_iter()
            .filter(|path| matches_glob(&pattern, relative_to(&root, path, root_is_file)))
            .collect();
        matches.sort();

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

pub struct GrepTool {
    working_dir: PathBuf,
}

impl GrepTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for" },
                "path": { "type": "string", "format": "path", "description": "Optional directory to search" },
                "include": { "type": "string", "description": "Optional glob restricting searched files" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: GrepArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Grep: {error}"))?;
        let root = resolve_path(&self.working_dir, args.path.as_deref().unwrap_or("."));
        let regex = Regex::new(&args.pattern)
            .map_err(|error| format!("Invalid regular expression: {error}"))?;
        let include = args
            .include
            .as_deref()
            .map(Pattern::new)
            .transpose()
            .map_err(|error| format!("Invalid include pattern: {error}"))?;
        let root_is_file = root.is_file();
        let mut files = Vec::new();
        collect_files(&root, &mut files)?;
        files.sort();

        let mut results = Vec::new();
        for path in files {
            let relative = relative_to(&root, &path, root_is_file);
            if include
                .as_ref()
                .is_some_and(|pattern| !matches_glob(pattern, relative))
            {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            for (index, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    results.push(format!("{}:{}: {line}", path.display(), index + 1));
                    if results.len() == MAX_RESULTS {
                        return Ok(format!("{}\n[results truncated]", results.join("\n")));
                    }
                }
            }
        }
        Ok(results.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn glob_matches_files_recursively() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn example() {}").unwrap();
        fs::write(root.join("src/lib.txt"), "example").unwrap();

        let tool = GlobTool::new(&root);
        let result = tool.execute(r#"{"pattern":"**/*.rs"}"#).await.unwrap();
        assert_eq!(result, root.join("src/lib.rs").display().to_string());

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn edit_returns_unified_diff() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file.txt"), "one\nbefore\nthree\n").unwrap();

        let tool = EditTool::new(&root);
        let result = tool
            .execute(r#"{"path":"file.txt","old_string":"before","new_string":"after"}"#)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(root.join("file.txt")).unwrap(),
            "one\nafter\nthree\n"
        );
        assert_eq!(
            result,
            format!(
                "--- {0}\n+++ {0}\n@@ -1,3 +1,3 @@\n one\n-before\n+after\n three",
                root.join("file.txt").display()
            )
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn edit_replaces_all_when_requested() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file.txt"), "before before").unwrap();

        let tool = EditTool::new(&root);
        tool.execute(
            r#"{"path":"file.txt","old_string":"before","new_string":"after","replace_all":true}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("file.txt")).unwrap(),
            "after after"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn edit_rejects_unchanged_content() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file.txt"), "before").unwrap();

        let tool = EditTool::new(&root);
        let error = tool
            .execute(r#"{"path":"file.txt","old_string":"before","new_string":"before"}"#)
            .await
            .unwrap_err();
        assert_eq!(error, "old_string and new_string must differ");

        fs::remove_dir_all(root).unwrap();
    }
}
