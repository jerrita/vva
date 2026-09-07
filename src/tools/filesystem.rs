use async_trait::async_trait;
use glob::{MatchOptions, Pattern};
use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::Tool;

const MAX_RESULTS: usize = 1_000;
const MAX_BRACE_ALTS: usize = 256;
const GLOB_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

fn strip_wrapping_quotes(mut value: &str) -> &str {
    loop {
        let bytes = value.as_bytes();
        if bytes.len() < 2 {
            return value;
        }
        let quote = bytes[0];
        if (quote == b'"' || quote == b'\'') && bytes[bytes.len() - 1] == quote {
            value = &value[1..value.len() - 1];
            continue;
        }
        return value;
    }
}

pub fn resolve_path(working_dir: &Path, path: &str) -> PathBuf {
    let path = strip_wrapping_quotes(path);
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

fn expand_braces(pattern: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    expand_braces_into(pattern, &mut out)?;
    Ok(out)
}

fn expand_braces_into(pattern: &str, out: &mut Vec<String>) -> Result<(), String> {
    if out.len() >= MAX_BRACE_ALTS {
        return Err(format!(
            "brace expansion exceeded {MAX_BRACE_ALTS} alternatives"
        ));
    }
    match next_expandable_group(pattern) {
        None => {
            out.push(pattern.to_string());
            Ok(())
        }
        Some((start, end, alts)) => {
            let prefix = &pattern[..start];
            let suffix = &pattern[end..];
            for alt in alts {
                expand_braces_into(&format!("{prefix}{alt}{suffix}"), out)?;
            }
            Ok(())
        }
    }
}

fn next_expandable_group(pattern: &str) -> Option<(usize, usize, Vec<String>)> {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'{'
            && let Some((end, alts)) = parse_brace_alts(pattern, i)
        {
            return Some((i, end, alts));
        }
        i += 1;
    }
    None
}

fn parse_brace_alts(pattern: &str, start: usize) -> Option<(usize, Vec<String>)> {
    let bytes = pattern.as_bytes();
    let mut depth = 0;
    let mut alt_start = start + 1;
    let mut alts = Vec::new();
    let mut has_comma = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 1;
                if i < bytes.len() {
                    i += 1;
                }
                continue;
            }
            b'{' => depth += 1,
            b',' if depth == 1 => {
                has_comma = true;
                alts.push(pattern[alt_start..i].to_string());
                alt_start = i + 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if !has_comma {
                        return None;
                    }
                    alts.push(pattern[alt_start..i].to_string());
                    return Some((i + 1, alts));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_glob_patterns(pattern: &str) -> Result<Vec<Pattern>, String> {
    expand_braces(strip_wrapping_quotes(pattern))?
        .into_iter()
        .map(|item| Pattern::new(&item).map_err(|error| error.to_string()))
        .collect()
}

fn matches_any(patterns: &[Pattern], path: &Path) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern.matches_path_with(path, GLOB_OPTIONS))
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
        r#"Read an existing UTF-8 text file; use Glob to locate files and Grep to search their contents. Returns lines prefixed with 1-indexed line numbers. Use offset and limit for a specific range; by default the entire file is returned. An offset beyond EOF returns empty text. Directories and non-UTF-8 files produce errors. Numbered output larger than 32 KiB is saved to the project tool_outputs directory; the result then contains that path so you can Read or Grep it.
Paths are absolute or relative to the workspace root; shell variables and ~ are not expanded. Example arguments: {"path":"src/main.rs","offset":1,"limit":200}."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "Existing text file path, absolute or workspace-relative." },
                "offset": { "type": "integer", "description": "First line to read, 1-indexed. Omit to start at line 1; use a positive integer." },
                "limit": { "type": "integer", "description": "Maximum number of lines to return. Omit to read through EOF; 0 returns no lines." }
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
struct WriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        r#"Create a new UTF-8 text file with complete contents. Use Edit to change an existing file. The parent directory must already exist; this tool does not create directories and fails if the target exists. Returns the created file path.
Paths are absolute or relative to the workspace root; shell variables and ~ are not expanded. Example arguments: {"path":"src/example.rs","content":"pub fn example() {}\n"}."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "New file path, absolute or workspace-relative, with an existing parent directory." },
                "content": { "type": "string", "description": "Complete literal file contents. Use JSON escapes for newlines and quotes; do not include Markdown fences unless they belong in the file." }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: WriteArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Write: {error}"))?;
        let path = resolve_path(&self.working_dir, &args.path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("Failed to create file {}: {error}", path.display()))?;
        file.write_all(args.content.as_bytes())
            .map_err(|error| format!("Failed to write file {}: {error}", path.display()))?;
        Ok(format!("Created file {}", path.display()))
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
        r#"Replace exact text in an existing UTF-8 file. Read the file first and copy the target text without the displayed line-number prefixes. Matching is literal, including whitespace and newlines; regular expressions are not supported. By default old_string must occur exactly once; include enough context to make it unique, or set replace_all to true to replace every occurrence. Fails if old_string is empty, absent, or identical to new_string. Returns a unified diff after writing the change.
Paths are absolute or relative to the workspace root; shell variables and ~ are not expanded. Example arguments: {"path":"src/example.rs","old_string":"let count = 1;","new_string":"let count = 2;"}."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "Existing text file path, absolute or workspace-relative." },
                "old_string": { "type": "string", "description": "Nonempty literal text to replace, preserving exact whitespace. Must match once unless replace_all is true. Exclude Read's line-number prefixes." },
                "new_string": { "type": "string", "description": "Literal replacement text, different from old_string. Use an empty string to delete the matched text." },
                "replace_all": { "type": "boolean", "description": "Defaults to false, requiring exactly one match. Set true only when every occurrence should be replaced." }
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
        r#"Find files by path pattern; use Grep to search file contents. Matches are case-sensitive and relative to the search root. Supports *, ?, character classes such as [ab], ** as a full path component for recursive matching, and brace expansion such as **/*.{rs,md,toml}. A single * does not cross directory separators. Extglobs are unsupported.
Returns sorted file paths, at most 1000, with a truncation marker when more match. Empty output means no matches. Hidden and gitignored files are included; directory symlinks are not traversed during recursion. Example arguments: {"pattern":"**/*.{rs,md}","path":"src"}. Shell variables and ~ are not expanded."#
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
        let args: GlobArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Glob: {error}"))?;
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
        matches.sort();
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
        r#"Search UTF-8 file contents line by line with a Rust regular expression; use Glob to find files by name. Matching is case-sensitive by default; use (?i) for case-insensitive search. Supports alternation such as foo|bar, but not lookaround or backreferences. Matches cannot span lines. Non-UTF-8 and unreadable files are skipped.
Optionally restrict files with include, a glob relative to the search root. Glob syntax supports *, ?, character classes, recursive **, and brace expansion such as **/*.{rs,md,toml}. Extglobs are unsupported. Hidden and gitignored files are included; directory symlinks are not traversed during recursion.
Returns path:line: text with 1-indexed line numbers, capped at 1000 matching lines with a truncation marker. Empty output means no matches. Example arguments: {"pattern":"fn\\s+main|struct","path":"src","include":"**/*.{rs,toml}"}. JSON-escape regex backslashes. Path values do not expand shell variables or ~."#
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
        let args: GrepArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Grep: {error}"))?;
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
        files.sort();

        let mut results = Vec::new();
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
        let quoted = tool.execute(r#"{"pattern":"\"**/*.rs\""}"#).await.unwrap();
        assert_eq!(quoted, root.join("src/lib.rs").display().to_string());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strip_wrapping_quotes_peels_paired_quotes() {
        assert_eq!(strip_wrapping_quotes(r#""src/lib.rs""#), "src/lib.rs");
        assert_eq!(strip_wrapping_quotes("'src/lib.rs'"), "src/lib.rs");
        assert_eq!(strip_wrapping_quotes(r#""'src/lib.rs'""#), "src/lib.rs");
        assert_eq!(strip_wrapping_quotes("src/lib.rs"), "src/lib.rs");
        assert_eq!(strip_wrapping_quotes(r#""src/lib.rs"#), r#""src/lib.rs"#);
    }

    #[tokio::test]
    async fn read_strips_quoted_path() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "hello").unwrap();

        let tool = ReadTool::new(&root);
        let result = tool.execute(r#"{"path":"\"src/lib.rs\""}"#).await.unwrap();
        assert_eq!(result, "1: hello");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_braces_splits_comma_groups() {
        assert_eq!(
            expand_braces("**/*.{rs,md,toml}").unwrap(),
            ["**/*.rs", "**/*.md", "**/*.toml"]
        );
        assert_eq!(expand_braces("Cargo.toml").unwrap(), ["Cargo.toml"]);
        assert_eq!(expand_braces("{a}").unwrap(), ["{a}"]);
        assert_eq!(expand_braces("{a,{b,c}}").unwrap(), ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn glob_expands_brace_alternatives() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn example() {}").unwrap();
        fs::write(root.join("src/readme.md"), "# readme").unwrap();
        fs::write(root.join("src/lib.txt"), "example").unwrap();

        let tool = GlobTool::new(&root);
        let result = tool.execute(r#"{"pattern":"**/*.{rs,md}"}"#).await.unwrap();
        let mut lines: Vec<String> = result.lines().map(str::to_owned).collect();
        lines.sort();
        let mut expected = vec![
            root.join("src/lib.rs").display().to_string(),
            root.join("src/readme.md").display().to_string(),
        ];
        expected.sort();
        assert_eq!(lines, expected);

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn grep_include_expands_braces() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "needle in rust").unwrap();
        fs::write(root.join("src/readme.md"), "needle in markdown").unwrap();
        fs::write(root.join("src/lib.txt"), "needle in text").unwrap();

        let tool = GrepTool::new(&root);
        let result = tool
            .execute(r#"{"pattern":"needle","include":"**/*.{rs,md}"}"#)
            .await
            .unwrap();
        assert!(result.contains("src/lib.rs"));
        assert!(result.contains("src/readme.md"));
        assert!(!result.contains("src/lib.txt"));

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
    async fn write_creates_a_new_file() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        let tool = WriteTool::new(&root);
        let result = tool
            .execute(r#"{"path":"file.txt","content":"hello\nworld"}"#)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(root.join("file.txt")).unwrap(),
            "hello\nworld"
        );
        assert_eq!(
            result,
            format!("Created file {}", root.join("file.txt").display())
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn write_rejects_existing_file() {
        let root = std::env::temp_dir().join(format!("vva-tools-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file.txt"), "existing").unwrap();

        let tool = WriteTool::new(&root);
        let error = tool
            .execute(r#"{"path":"file.txt","content":"replacement"}"#)
            .await
            .unwrap_err();

        assert!(error.starts_with("Failed to create file "));
        assert_eq!(
            fs::read_to_string(root.join("file.txt")).unwrap(),
            "existing"
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
