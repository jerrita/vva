use crate::config::resolver::InputCapabilities;
use crate::model::{MediaContent, MediaKind, ToolCall, ToolResult};
use async_trait::async_trait;
use base64::Engine;
use glob::{MatchOptions, Pattern};
use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::Tool;

const MAX_RESULTS: usize = 100;
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

fn sort_by_mtime(files: &mut [PathBuf]) -> Result<(), String> {
    let mut modified = std::collections::HashMap::new();
    for path in files.iter() {
        let time = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                format!(
                    "Failed to read modification time {}: {error}",
                    path.display()
                )
            })?;
        modified.insert(path.clone(), time);
    }
    files.sort_by(|a, b| modified[b].cmp(&modified[a]).then_with(|| a.cmp(b)));
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
    inputs: InputCapabilities,
}

impl ReadTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            inputs: InputCapabilities::default(),
        }
    }

    pub fn with_inputs(mut self, inputs: InputCapabilities) -> Self {
        self.inputs = inputs;
        self
    }

    async fn read(&self, arguments: &str) -> Result<(String, Vec<MediaContent>), String> {
        let args: ReadArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Read: {error}"))?;
        let path = resolve_path(&self.working_dir, &args.path);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|error| format!("Failed to read file {}: {error}", path.display()))?;
        if let Some((kind, mime_type)) = media_type(&path, &bytes) {
            self.inputs.require(kind, mime_type)?;
            if args.offset.is_some() || args.limit.is_some() {
                return Err("offset and limit apply only to text files".into());
            }
            let media = MediaContent {
                kind,
                mime_type: mime_type.into(),
                filename: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            };
            return Ok((
                format!(
                    "Read {} ({mime_type}, {} bytes)",
                    path.display(),
                    bytes.len()
                ),
                vec![media],
            ));
        }
        let content = std::str::from_utf8(&bytes)
            .map_err(|_| format!("Unsupported binary file: {}", path.display()))?;
        if content.contains('\0') {
            return Err(format!("Unsupported binary file: {}", path.display()));
        }
        Ok((read_text(content, &args), Vec::new()))
    }
}

fn media_type(path: &Path, bytes: &[u8]) -> Option<(MediaKind, &'static str)> {
    use MediaKind::*;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((Image, "image/png"));
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some((Image, "image/jpeg"));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some((Image, "image/gif"));
    }
    if bytes.starts_with(b"%PDF-") {
        return Some((Pdf, "application/pdf"));
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => (Image, "image/png"),
        "jpg" | "jpeg" => (Image, "image/jpeg"),
        "gif" => (Image, "image/gif"),
        "webp" => (Image, "image/webp"),
        "heic" => (Image, "image/heic"),
        "heif" => (Image, "image/heif"),
        "avif" => (Image, "image/avif"),
        "pdf" => (Pdf, "application/pdf"),
        "wav" => (Audio, "audio/wav"),
        "mp3" => (Audio, "audio/mpeg"),
        "aiff" | "aif" => (Audio, "audio/aiff"),
        "aac" => (Audio, "audio/aac"),
        "ogg" | "oga" => (Audio, "audio/ogg"),
        "flac" => (Audio, "audio/flac"),
        "m4a" => (Audio, "audio/mp4"),
        "mp4" | "m4v" => (Video, "video/mp4"),
        "mpeg" | "mpg" => (Video, "video/mpeg"),
        "mov" => (Video, "video/quicktime"),
        "avi" => (Video, "video/x-msvideo"),
        "webm" => (Video, "video/webm"),
        "wmv" => (Video, "video/x-ms-wmv"),
        "3gp" => (Video, "video/3gpp"),
        _ => return None,
    })
}

fn read_text(content: &str, args: &ReadArgs) -> String {
    let lines: Vec<_> = content.lines().collect();
    let start = args.offset.unwrap_or(1).saturating_sub(1);
    let end = args
        .limit
        .map(|limit| start.saturating_add(limit))
        .unwrap_or(lines.len())
        .min(lines.len());
    if start >= lines.len() {
        return format!("[End of file: {} lines]", lines.len());
    }
    let mut output = String::new();
    let mut next = start;
    for (index, line) in lines.iter().enumerate().take(end).skip(start) {
        let numbered = format!(
            "{}{}: {line}",
            if output.is_empty() { "" } else { "\n" },
            index + 1
        );
        let remaining = super::MAX_INLINE_OUTPUT_BYTES - output.len();
        if numbered.len() > remaining {
            // Keep the whole prefix within the byte budget, including a partial line.
            let mut cut = remaining;
            while !numbered.is_char_boundary(cut) {
                cut -= 1;
            }
            output.push_str(&numbered[..cut]);
            output.push_str(&format!("\n[32 KiB limit: {} total lines; continue with Read offset={} to reread the truncated line.{}]", lines.len(), index + 1, if line.len() > super::MAX_INLINE_OUTPUT_BYTES { " This line exceeds the byte limit; use another tool for its remainder." } else { "" }));
            return output;
        }
        output.push_str(&numbered);
        next = index + 1;
    }
    if next < lines.len() {
        output.push_str(&format!(
            "\n[{} total lines; continue with Read offset={}.]",
            lines.len(),
            next + 1
        ));
    } else {
        output.push_str(&format!("\n[End of file: {} lines]", lines.len()));
    }
    output
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
        r#"Read an existing file; use Glob to locate files and Grep to search their contents. UTF-8 text is returned with 1-indexed line numbers. Use offset and limit for a range. Text is capped at 32 KiB; only the excess is truncated, with a pagination hint appended at the end. A line cut by the byte limit is marked; use another tool to inspect its remainder. Binary images, PDFs, audio and video are returned as multimodal content when the current model's configured input modalities and native protocol support them; text pagination arguments do not apply. Unsupported binary files and directories produce errors. Issue independent Read calls in parallel when reading multiple files.
Paths are absolute or relative to the workspace root; shell variables and ~ are not expanded. Example arguments: {"path":"src/main.rs","offset":1,"limit":200}."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "Existing file path, absolute or workspace-relative." },
                "offset": { "type": "integer", "description": "First line to read, 1-indexed. Omit to start at line 1; use a positive integer." },
                "limit": { "type": "integer", "description": "Maximum number of lines to return. Omit to read through EOF or the byte limit; 0 returns only the pagination hint." }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let (content, attachments) = self.read(arguments).await?;
        if !attachments.is_empty() {
            return Err("Use execute_result to receive multimodal content".into());
        }
        Ok(content)
    }

    async fn execute_result(&self, call: &ToolCall) -> ToolResult {
        match self.read(&call.arguments).await {
            Ok((content, attachments)) => ToolResult {
                tool_call_id: call.id.clone(),
                content,
                attachments,
                is_error: false,
            },
            Err(error) => ToolResult::error(&call.id, error),
        }
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
        r#"Replace exact text in an existing UTF-8 file. Read the file first and copy the target text without the displayed line-number prefixes. Matching is literal, including whitespace and newlines; regular expressions are not supported. By default old_string must occur exactly once; include enough context to make it unique, or set replace_all to true to replace every occurrence. An empty old_string overwrites the entire file with new_string. CRLF is normalized to LF in file contents and both strings. Fails if nonempty old_string is absent or identical to new_string. Returns a unified diff after writing the change.
Paths are absolute or relative to the workspace root; shell variables and ~ are not expanded. Example arguments: {"path":"src/example.rs","old_string":"let count = 1;","new_string":"let count = 2;"}."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "Existing text file path, absolute or workspace-relative." },
                "old_string": { "type": "string", "description": "Literal text to replace; empty overwrites the entire file. CRLF is normalized to LF. Must match once unless replace_all is true. Exclude Read's line-number prefixes." },
                "new_string": { "type": "string", "description": "Literal replacement text, different from old_string. Use an empty string to delete the matched text." },
                "replace_all": { "type": "boolean", "description": "Defaults to false, requiring exactly one match. Set true only when every occurrence should be replaced." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: EditArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Edit: {error}"))?;
        let old_string = args.old_string.replace("\r\n", "\n");
        let new_string = args.new_string.replace("\r\n", "\n");
        if !old_string.is_empty() && old_string == new_string {
            return Err("old_string and new_string must differ".to_string());
        }

        let path = resolve_path(&self.working_dir, &args.path);
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("Failed to read file {}: {error}", path.display()))?;
        let content = content.replace("\r\n", "\n");
        if old_string.is_empty() {
            fs::write(&path, &new_string)
                .map_err(|error| format!("Failed to write file {}: {error}", path.display()))?;
            return Ok(unified_diff(&path, &content, &new_string));
        }
        let matches = content.match_indices(&old_string).count();
        if matches == 0 {
            return Err("old_string was not found".to_string());
        }
        if !args.replace_all.unwrap_or(false) && matches != 1 {
            return Err(format!(
                "old_string must occur exactly once, found {matches}; use replace_all to replace every occurrence"
            ));
        }

        let updated = if args.replace_all.unwrap_or(false) {
            content.replace(&old_string, &new_string)
        } else {
            content.replacen(&old_string, &new_string, 1)
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
Returns path:line: text with 1-indexed line numbers, sorted by file modification time (newest first), capped at 100 matching lines with a truncation marker. Empty output means no matches. Example arguments: {"pattern":"fn\\s+main|struct","path":"src","include":"**/*.{rs,toml}"}. JSON-escape regex backslashes. Path values do not expand shell variables or ~."#
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
        sort_by_mtime(&mut files)?;

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
                    if results.len() == MAX_RESULTS {
                        return Ok(format!("{}\n[results truncated]", results.join("\n")));
                    }
                    results.push(format!("{}:{}: {line}", path.display(), index + 1));
                }
            }
        }
        Ok(results.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_preserves_prefix_and_reports_pagination() {
        let args = ReadArgs {
            path: "sample".into(),
            offset: None,
            limit: Some(1),
        };
        assert_eq!(
            read_text("alpha\nbeta", &args),
            "1: alpha\n[2 total lines; continue with Read offset=2.]"
        );
        let args = ReadArgs {
            limit: None,
            ..args
        };
        let output = read_text(&"界".repeat(super::super::MAX_INLINE_OUTPUT_BYTES), &args);
        let prefix = output.split("\n[").next().unwrap();
        assert_eq!(prefix.len(), super::super::MAX_INLINE_OUTPUT_BYTES - 2);
        assert!(prefix.starts_with("1: 界"));
        assert!(output.contains("line exceeds the byte limit"));
    }

    #[tokio::test]
    async fn read_binary_is_gated_and_persists_bytes() {
        let root = std::env::temp_dir().join(format!("vva-media-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=";
        fs::write(
            root.join("sample.png"),
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap(),
        )
        .unwrap();
        let call = ToolCall::new("image_read", "Read", r#"{"path":"sample.png"}"#);
        assert!(ReadTool::new(&root).execute_result(&call).await.is_error);
        let mut registry =
            crate::tools::ToolRegistry::with_workspace_defaults(&root, Some(root.join("outputs")));
        registry.register(std::sync::Arc::new(ReadTool::new(&root).with_inputs(
            InputCapabilities {
                model: "test/vision".into(),
                modalities: vec!["image".into()],
                protocol: crate::config::ProviderType::Response,
            },
        )));
        let result = registry.execute_call(&call).await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(result.attachments[0].data, data);
        assert_eq!(result.attachments[0].kind, MediaKind::Image);
        let restored: ToolResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(restored, result);
    }

    #[tokio::test]
    async fn edit_overwrites_and_normalizes_crlf() {
        let root = std::env::temp_dir().join(format!("vva-edit-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "alpha\r\nbeta\r\n").unwrap();
        let tool = EditTool::new(&root);
        tool.execute(
            r#"{"path":"sample","old_string":"alpha\nbeta","new_string":"gamma\r\ndelta"}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sample")).unwrap(),
            "gamma\ndelta\n"
        );
        tool.execute(r#"{"path":"sample","old_string":"","new_string":"reset\r\n"}"#)
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("sample")).unwrap(), "reset\n");
        tool.execute(r#"{"path":"sample","old_string":"","new_string":""}"#)
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("sample")).unwrap(), "");
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn searches_sort_before_capping_and_only_mark_excess() {
        let root = std::env::temp_dir().join(format!("vva-search-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        for index in 0..=100 {
            let path = root.join(format!("{index:03}.txt"));
            fs::write(&path, if index == 0 { "absent" } else { "match" }).unwrap();
            fs::File::open(path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(index))
                .unwrap();
        }
        let glob = GlobTool::new(&root)
            .execute(r#"{"pattern":"*.txt"}"#)
            .await
            .unwrap();
        assert!(glob.lines().next().unwrap().ends_with("100.txt"));
        assert!(glob.lines().nth(99).unwrap().ends_with("001.txt"));
        assert_eq!(glob.lines().last(), Some("[results truncated]"));
        let tool = GrepTool::new(&root);
        let grep = tool.execute(r#"{"pattern":"match"}"#).await.unwrap();
        assert_eq!(grep.lines().count(), 100);
        assert!(grep.lines().next().unwrap().ends_with("100.txt:1: match"));
        let path = root.join("000.txt");
        fs::write(&path, "match").unwrap();
        fs::File::open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH)
            .unwrap();
        let grep = tool.execute(r#"{"pattern":"match"}"#).await.unwrap();
        assert_eq!(grep.lines().count(), 101);
        assert_eq!(grep.lines().last(), Some("[results truncated]"));
        fs::remove_dir_all(root).unwrap();
    }

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
        assert_eq!(result, "1: hello\n[End of file: 1 lines]");

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
