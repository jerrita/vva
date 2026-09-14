use crate::config::resolver::InputCapabilities;
use crate::model::{EditorKind, MediaContent, MediaKind, ToolCall, ToolResult};
use async_trait::async_trait;
use base64::Engine;
use glob::{MatchOptions, Pattern};
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

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

fn unified_diff(
    path: &Path,
    original: &str,
    updated: &str,
    old_offset: usize,
    new_offset: usize,
) -> String {
    let original_lines: Vec<_> = original.split_inclusive('\n').collect();
    let updated_lines: Vec<_> = updated.split_inclusive('\n').collect();

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
        format_range(old_offset + context_start, original_count),
        format_range(new_offset + context_start, updated_count),
    );
    let mut push_line = |marker: char, line: &str| {
        diff.push_str(&format!(
            "\n{marker}{}",
            line.strip_suffix('\n').unwrap_or(line)
        ));
        if !line.ends_with('\n') {
            diff.push_str("\n\\ No newline at end of file");
        }
    };
    for line in &original_lines[context_start..prefix] {
        push_line(' ', line);
    }
    for line in &original_lines[prefix..original_changed_end] {
        push_line('-', line);
    }
    for line in &updated_lines[prefix..updated_changed_end] {
        push_line('+', line);
    }
    for line in &original_lines[original_changed_end..original_end] {
        push_line(' ', line);
    }
    diff
}

pub struct ReadTool {
    working_dir: PathBuf,
    inputs: InputCapabilities,
    editor: EditorKind,
    hashline: Option<std::sync::Arc<super::hashline::HashlineSession>>,
}

impl ReadTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            inputs: InputCapabilities::default(),
            editor: EditorKind::Replace,
            hashline: None,
        }
    }

    pub fn with_inputs(mut self, inputs: InputCapabilities) -> Self {
        self.inputs = inputs;
        self
    }

    pub fn with_editor(mut self, editor: EditorKind) -> Self {
        self.editor = editor;
        self
    }

    pub fn with_hashline(
        mut self,
        session: std::sync::Arc<super::hashline::HashlineSession>,
    ) -> Self {
        self.hashline = Some(session);
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
            if args.offset.is_some() || args.limit.is_some() || args.byte_offset.is_some() {
                return Err("offset, limit and byte_offset apply only to text files".into());
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
        if self.editor == EditorKind::Hashline {
            let canonical = tokio::fs::canonicalize(&path)
                .await
                .unwrap_or_else(|_| path.clone());
            return Ok((
                super::hashline::format_read(
                    &args.path,
                    content,
                    args.offset,
                    args.limit,
                    args.byte_offset,
                    self.hashline.as_deref(),
                    &canonical,
                )?,
                Vec::new(),
            ));
        }
        Ok((format_read_text(content, &args, self.editor)?, Vec::new()))
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

pub(crate) fn content_version(content: &str) -> String {
    // 64-bit truncation suffices for optimistic stale detection within a session.
    format!("{:x}", Sha256::digest(content.as_bytes()))[..16].to_string()
}

fn format_read_text(content: &str, args: &ReadArgs, editor: EditorKind) -> Result<String, String> {
    let offset = args.offset.unwrap_or(1);
    if offset == 0 {
        return Err("offset must be a positive, 1-indexed line number".into());
    }
    if args.byte_offset.is_some() && args.limit != Some(1) {
        return Err("byte_offset requires limit=1".into());
    }
    // Reserve space for the version and continuation notice as well as the text.
    let budget = super::MAX_INLINE_OUTPUT_BYTES - 256;
    let mut output = if editor == EditorKind::Replace {
        format!("[version: {}]\n", content_version(content))
    } else {
        String::new()
    };
    let all_lines: Vec<&str> = content.lines().collect();
    let mut lines = all_lines
        .iter()
        .copied()
        .enumerate()
        .skip(offset - 1)
        .peekable();
    let mut shown = 0;
    while shown < args.limit.unwrap_or(usize::MAX) {
        let Some((index, line)) = lines.next() else {
            if args.byte_offset.is_some() {
                return Err("byte_offset refers to a line past EOF".into());
            }
            output.push_str("[End of file]");
            return Ok(output);
        };
        let byte_offset = args.byte_offset.unwrap_or(0);
        if byte_offset > line.len() || !line.is_char_boundary(byte_offset) {
            return Err(format!(
                "byte_offset must be a UTF-8 boundary within line {} ({} bytes)",
                index + 1,
                line.len()
            ));
        }
        let prefix = line_prefix(index, byte_offset);
        let text = &line[byte_offset..];
        let remaining = budget.saturating_sub(output.len());
        if prefix.len() + text.len() + 1 > remaining {
            if shown > 0 {
                output.push_str(&format!("[Continue with Read offset={}.]", index + 1));
                return Ok(output);
            }
            let mut cut = remaining.saturating_sub(prefix.len() + 1).min(text.len());
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            output.push_str(&prefix);
            output.push_str(&text[..cut]);
            output.push_str(&format!(
                "\n[Partial line; continue with Read offset={}, byte_offset={}, limit=1.]",
                index + 1,
                byte_offset + cut
            ));
            return Ok(output);
        }
        output.push_str(&prefix);
        output.push_str(text);
        output.push('\n');
        shown += 1;
    }
    match lines.peek() {
        Some((index, _)) => output.push_str(&format!("[Continue with Read offset={}.]", index + 1)),
        None => output.push_str("[End of file]"),
    }
    Ok(output)
}

fn line_prefix(index: usize, byte_offset: usize) -> String {
    if byte_offset == 0 {
        format!("{}: ", index + 1)
    } else {
        format!("{}@{byte_offset}: ", index + 1)
    }
}

#[cfg(test)]
fn read_text(content: &str, args: &ReadArgs) -> Result<String, String> {
    format_read_text(content, args, EditorKind::Replace)
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    byte_offset: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        match self.editor {
            EditorKind::Hashline => {
                r#"Read an existing file; use Glob to locate files and Grep to search contents. UTF-8 text starts with [PATH#TAG] then N:line (1-indexed, no space after the colon). Copy the header into Edit; numbers are that snapshot. Grep uses the same headers. offset and limit are 1-indexed line counts. Output is capped at 32 KiB and stops at complete lines. An oversized line is marked Partial; continue it using the returned byte_offset with the same offset and limit=1. Byte offsets are 0-indexed UTF-8 byte positions within that line. Partial and elided lines are unseen to Edit. Binary images, PDFs, audio and video are returned as multimodal content when supported by the current model and protocol; text pagination arguments do not apply. Unsupported binary files and directories produce errors. Issue independent Read calls in parallel.
Paths are absolute or workspace-relative; shell variables and ~ are not expanded. Example: {"path":"src/main.rs","offset":1,"limit":200}."#
            }
            EditorKind::Patch => {
                r#"Read an existing file; use Glob to locate files and Grep to search contents. UTF-8 text has 1-indexed line numbers. Use offset and limit for a range. Output is capped at 32 KiB and stops at complete lines. An oversized line is marked Partial; continue it using the returned byte_offset with the same offset and limit=1. Byte offsets are 0-indexed UTF-8 byte positions within that line. Copy code without line-number prefixes or notices into Edit hunks. Binary images, PDFs, audio and video are returned as multimodal content when supported by the current model and protocol; text pagination arguments do not apply. Unsupported binary files and directories produce errors. Issue independent Read calls in parallel.
Paths are absolute or workspace-relative; shell variables and ~ are not expanded. Example: {"path":"src/main.rs","offset":1,"limit":200}."#
            }
            EditorKind::Replace => {
                r#"Read an existing file; use Glob to locate files and Grep to search contents. UTF-8 text has 1-indexed line numbers and a content version; copy that version into Edit's expected_version to detect changes since this read. Use offset and limit for a range. Output is capped at 32 KiB and stops at complete lines. An oversized line is marked Partial; continue it using the returned byte_offset with the same offset and limit=1. Byte offsets are 0-indexed UTF-8 byte positions within that line. Copy code without line-number prefixes or notices. Binary images, PDFs, audio and video are returned as multimodal content when supported by the current model and protocol; text pagination arguments do not apply. Unsupported binary files and directories produce errors. Issue independent Read calls in parallel.
Paths are absolute or workspace-relative; shell variables and ~ are not expanded. Example: {"path":"src/main.rs","offset":1,"limit":200}."#
            }
        }
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "format": "path", "description": "Existing file path, absolute or workspace-relative." },
                "offset": { "type": "integer", "description": "First line to read, 1-indexed. Omit to start at line 1; use a positive integer." },
                "limit": { "type": "integer", "description": "Maximum lines to return. Omit to read through EOF or the byte limit; 0 returns metadata only." },
                "byte_offset": { "type": "integer", "description": "0-indexed UTF-8 byte position within the selected line; requires limit=1. Copy from a Partial line continuation notice." }
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
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    expected_version: Option<String>,
    edits: Option<Vec<Replacement>>,
    old_string: Option<String>,
    new_string: Option<String>,
    replace_all: Option<bool>,
    expected_matches: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
    expected_matches: Option<usize>,
}

impl EditArgs {
    fn replacements(self) -> Result<Vec<Replacement>, String> {
        if let Some(edits) = self.edits {
            if self.old_string.is_some()
                || self.new_string.is_some()
                || self.replace_all.is_some()
                || self.expected_matches.is_some()
            {
                return Err(
                    "invalid_arguments: use either edits[] or top-level replacement fields".into(),
                );
            }
            if edits.is_empty() {
                return Err(
                    "invalid_arguments: edits[] must contain at least one replacement".into(),
                );
            }
            return Ok(edits);
        }
        Ok(vec![Replacement {
            old_string: self
                .old_string
                .ok_or("invalid_arguments: old_string is required for a single replacement")?,
            new_string: self
                .new_string
                .ok_or("invalid_arguments: new_string is required for a single replacement")?,
            replace_all: self.replace_all,
            expected_matches: self.expected_matches,
        }])
    }
}

// Weak entries share locks across registry/session instances without retaining files forever.
pub(crate) fn edit_lock(path: &Path) -> Result<Arc<Mutex<()>>, String> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "File mutation lock poisoned".to_string())?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = locks.get(path).and_then(Weak::upgrade).unwrap_or_default();
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    Ok(lock)
}

struct NormalizedText {
    text: String,
    crlf_offsets: Vec<usize>,
}

impl NormalizedText {
    fn new(raw: &str) -> Self {
        let crlf_offsets = raw
            .match_indices("\r\n")
            .enumerate()
            .map(|(removed, (offset, _))| offset - removed)
            .collect();
        Self {
            text: raw.replace("\r\n", "\n"),
            crlf_offsets,
        }
    }

    fn raw_offset(&self, offset: usize) -> usize {
        offset + self.crlf_offsets.partition_point(|&crlf| crlf < offset)
    }
}

struct MatchedReplacement {
    start: usize,
    end: usize,
    text: String,
    edit_index: usize,
}

#[derive(Debug)]
pub(crate) struct ChangedRange {
    pub old: std::ops::Range<usize>,
    pub new: std::ops::Range<usize>,
}

#[derive(Debug)]
pub(crate) struct EditPlan {
    pub updated: String,
    pub changes: Vec<ChangedRange>,
}

impl EditPlan {
    pub(crate) fn from_replacement(original: &str, updated: String) -> Self {
        let new_len = updated.len();
        Self {
            updated,
            changes: vec![ChangedRange {
                old: 0..original.len(),
                new: 0..new_len,
            }],
        }
    }
}

fn edit_diff(path: &Path, original: &str, plan: &EditPlan) -> String {
    fn line_offsets(text: &str) -> Vec<usize> {
        let mut offsets = vec![0];
        for line in text.split_inclusive('\n') {
            offsets.push(offsets.last().unwrap() + line.len());
        }
        offsets
    }
    fn window(offsets: &[usize], range: &std::ops::Range<usize>) -> std::ops::Range<usize> {
        let start = offsets
            .partition_point(|&offset| offset <= range.start)
            .saturating_sub(1);
        let end = offsets.partition_point(|&offset| offset < range.end);
        start.saturating_sub(3)..(end + 3).min(offsets.len() - 1)
    }
    let old_offsets = line_offsets(original);
    let new_offsets = line_offsets(&plan.updated);
    let mut groups: Vec<ChangedRange> = Vec::new();
    for change in &plan.changes {
        let old = window(&old_offsets, &change.old);
        let new = window(&new_offsets, &change.new);
        if let Some(last) = groups.last_mut()
            && (old.start <= last.old.end || new.start <= last.new.end)
        {
            last.old.end = last.old.end.max(old.end);
            last.new.end = last.new.end.max(new.end);
        } else {
            groups.push(ChangedRange { old, new });
        }
    }
    let mut output = String::new();
    for group in groups {
        let diff = unified_diff(
            path,
            &original[old_offsets[group.old.start]..old_offsets[group.old.end]],
            &plan.updated[new_offsets[group.new.start]..new_offsets[group.new.end]],
            group.old.start,
            group.new.start,
        );
        if output.is_empty() {
            output = diff;
        } else {
            output.push('\n');
            output.push_str(diff.splitn(3, '\n').nth(2).unwrap());
        }
    }
    output
}

fn local_line_ending(raw: &str, start: usize) -> &'static str {
    let newline = raw[start..]
        .find('\n')
        .map(|offset| start + offset)
        .or_else(|| raw[..start].rfind('\n'));
    if newline.is_some_and(|offset| offset > 0 && raw.as_bytes()[offset - 1] == b'\r') {
        "\r\n"
    } else {
        "\n"
    }
}

fn match_feedback(content: &str, needle: &str) -> String {
    let anchor = needle
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim);
    let mut candidates = Vec::new();
    if let Some(anchor) = anchor {
        for (index, line) in content.lines().enumerate() {
            if line.trim() == anchor {
                candidates.push(index + 1);
                if candidates.len() == 5 {
                    break;
                }
            }
        }
    }
    if candidates.is_empty() {
        "Use Grep to locate the target, then Read its current text.".into()
    } else {
        format!(
            "First-line candidates at lines {candidates:?}; Read around these lines and copy exact text."
        )
    }
}

fn apply_replacements(raw: &str, edits: &[Replacement]) -> Result<EditPlan, String> {
    let normalized = NormalizedText::new(raw);
    let content = &normalized.text;
    let mut matched = Vec::new();
    for (edit_index, edit) in edits.iter().enumerate() {
        let old = edit.old_string.replace("\r\n", "\n");
        let new = edit.new_string.replace("\r\n", "\n");
        if old.is_empty() {
            if edits.len() != 1
                || edit.replace_all.unwrap_or(false)
                || edit.expected_matches.is_some()
            {
                return Err("invalid_arguments: full-file replacement must be a single edit without match options".into());
            }
            // Explicit full-file content is used verbatim.
            return Ok(EditPlan {
                updated: edit.new_string.clone(),
                changes: vec![ChangedRange {
                    old: 0..raw.len(),
                    new: 0..edit.new_string.len(),
                }],
            });
        }
        if old == new {
            return Err(format!(
                "unchanged: edit {} has identical old_string and new_string",
                edit_index + 1
            ));
        }
        // Include overlapping occurrences in ambiguity detection.
        let mut positions = Vec::new();
        let mut search_from = 0;
        while let Some(found) = content[search_from..].find(&old) {
            let position = search_from + found;
            positions.push(position);
            search_from = position + old.chars().next().unwrap().len_utf8();
        }
        if positions.is_empty() {
            return Err(format!(
                "not_found: edit {} old_string was not found. {}",
                edit_index + 1,
                match_feedback(content, &old)
            ));
        }
        if edit
            .expected_matches
            .is_some_and(|expected| expected != positions.len())
        {
            return Err(format!(
                "match_count: edit {} expected {} matches, found {}",
                edit_index + 1,
                edit.expected_matches.unwrap(),
                positions.len()
            ));
        }
        if !edit.replace_all.unwrap_or(false) && positions.len() != 1 {
            let lines: Vec<_> = positions
                .iter()
                .take(5)
                .map(|&index| {
                    content[..index]
                        .bytes()
                        .filter(|&byte| byte == b'\n')
                        .count()
                        + 1
                })
                .collect();
            return Err(format!(
                "ambiguous: edit {} found {} matches at lines {lines:?}. Read around these lines and include unique context; use replace_all only when every occurrence is intended.",
                edit_index + 1,
                positions.len()
            ));
        }
        for position in positions {
            let start = normalized.raw_offset(position);
            let end = normalized.raw_offset(position + old.len());
            let ending = local_line_ending(raw, start);
            matched.push(MatchedReplacement {
                start,
                end,
                text: new.replace('\n', ending),
                edit_index,
            });
        }
    }
    matched.sort_by_key(|edit| edit.start);
    for pair in matched.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(format!(
                "overlap: edits {} and {} touch overlapping original text; merge them into one replacement",
                pair[0].edit_index + 1,
                pair[1].edit_index + 1
            ));
        }
    }
    let mut updated = String::with_capacity(raw.len());
    let mut changes = Vec::with_capacity(matched.len());
    let mut cursor = 0;
    for edit in matched {
        updated.push_str(&raw[cursor..edit.start]);
        let new_start = updated.len();
        updated.push_str(&edit.text);
        changes.push(ChangedRange {
            old: edit.start..edit.end,
            new: new_start..updated.len(),
        });
        cursor = edit.end;
    }
    updated.push_str(&raw[cursor..]);
    Ok(EditPlan { updated, changes })
}

pub(crate) fn persist_edit(path: &Path, original: &str, updated: &str) -> Result<(), String> {
    let permissions = fs::metadata(path)
        .map_err(|e| format!("Failed to inspect {}: {e}", path.display()))?
        .permissions();
    if permissions.readonly() {
        return Err(format!("File is read-only: {}", path.display()));
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("File has no parent directory")?)
            .map_err(|e| format!("Failed to stage edit: {e}"))?;
    temporary
        .write_all(updated.as_bytes())
        .map_err(|e| format!("Failed to stage edit: {e}"))?;
    temporary
        .as_file()
        .set_permissions(permissions)
        .map_err(|e| format!("Failed to preserve file permissions: {e}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| format!("Failed to sync edit: {e}"))?;
    let live = fs::read(path).map_err(|e| format!("Failed to verify file before writing: {e}"))?;
    if live != original.as_bytes() {
        return Err(
            "stale: file changed while preparing the edit; Read the current file and retry".into(),
        );
    }
    // Atomic replacement prevents partial files; external writers still need coordination.
    temporary
        .persist(path)
        .map_err(|e| format!("Failed to replace file: {}", e.error))?;
    Ok(())
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        r#"Replace exact text in an existing UTF-8 file. Read first; copy code without line-number prefixes. Use edits[] for multiple disjoint replacements in one file; every entry matches the ORIGINAL file. All entries are validated before any write, and overlapping matches fail. Each old_string must match once unless replace_all is true; expected_matches optionally checks the occurrence count. Matching preserves whitespace and Unicode; CRLF and LF match equivalently, while untouched bytes and existing line endings are preserved. Copy Read's version into expected_version to reject stale edits. A single empty old_string explicitly replaces the entire file with verbatim new_string and requires expected_version. Returns the actual diff and new version. Errors identify the failing edit and recovery action.
Use either edits[] or top-level old_string/new_string/replace_all/expected_matches. Paths are absolute or workspace-relative; shell variables and ~ are not expanded. Example: {"path":"src/example.rs","edits":[{"old_string":"let count = 1;","new_string":"let count = 2;"}]}."#
    }

    fn parameters(&self) -> serde_json::Value {
        let replacement = serde_json::json!({
            "old_string": { "type": "string", "description": "Exact original text, unique unless replace_all is true. A single empty string replaces the whole file and requires expected_version." },
            "new_string": { "type": "string", "description": "Replacement text; empty deletes the match." },
            "replace_all": { "type": "boolean", "description": "Replace every exact occurrence; defaults to false." },
            "expected_matches": { "type": "integer", "description": "Expected number of occurrences; mismatch rejects the edit." }
        });
        let mut properties = replacement.as_object().unwrap().clone();
        properties.insert("path".into(), serde_json::json!({"type":"string", "format":"path", "description":"Existing UTF-8 file path."}));
        properties.insert("expected_version".into(), serde_json::json!({"type":"string", "description":"Content version from Read or Edit; rejects changes since that version. Required for full-file replacement."}));
        properties.insert("edits".into(), serde_json::json!({
            "type":"array", "minItems":1,
            "description":"Disjoint replacements against the original file. Use instead of top-level replacement fields.",
            "items":{"type":"object", "properties":replacement, "required":["old_string","new_string"], "additionalProperties":false}
        }));
        serde_json::json!({"type":"object", "properties":properties, "required":["path"], "additionalProperties":false})
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: EditArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Edit: {error}"))?;
        let path = resolve_path(&self.working_dir, &args.path);
        let expected_version = args.expected_version.clone();
        let edits = args.replacements()?;
        if edits.iter().any(|edit| edit.old_string.is_empty()) && expected_version.is_none() {
            return Err(
                "invalid_arguments: full-file replacement requires expected_version from Read"
                    .into(),
            );
        }
        tokio::task::spawn_blocking(move || {
            commit_text_edit(&path, expected_version.as_deref(), true, |content| {
                apply_replacements(content, &edits)
            })
        })
        .await
        .map_err(|e| format!("Edit task failed: {e}"))?
    }
}

pub(crate) fn commit_text_edit(
    path: &Path,
    expected_version: Option<&str>,
    emit_version: bool,
    apply: impl FnOnce(&str) -> Result<EditPlan, String>,
) -> Result<String, String> {
    let path =
        fs::canonicalize(path).map_err(|e| format!("Failed to resolve {}: {e}", path.display()))?;
    let lock = edit_lock(&path)?;
    let _guard = lock
        .lock()
        .map_err(|_| "File mutation lock poisoned".to_string())?;
    let content =
        fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    if content.contains('\0') {
        return Err("Unsupported binary file".into());
    }
    let version = content_version(&content);
    if expected_version.is_some_and(|expected| expected != version) {
        return Err(format!(
            "stale: expected_version does not match current version {version}; Read the target range and retry"
        ));
    }
    let plan = apply(&content).map_err(|error| {
        if emit_version {
            format!("{error}\n[version: {version}]")
        } else {
            error
        }
    })?;
    if plan.updated == content {
        return Err("unchanged: edits produce no file changes".into());
    }
    persist_edit(&path, &content, &plan.updated)?;
    Ok(format_edit_result(&path, &content, &plan, emit_version))
}

pub(crate) fn format_edit_result(
    path: &Path,
    original: &str,
    plan: &EditPlan,
    emit_version: bool,
) -> String {
    let mut diff = edit_diff(path, original, plan);
    let budget = super::MAX_INLINE_OUTPUT_BYTES - 256;
    if diff.len() > budget {
        let mut end = budget;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
        diff.push_str("\n[Diff preview truncated; use Read for changed ranges.]");
    }
    if emit_version {
        format!("{diff}\n[version: {}]", content_version(&plan.updated))
    } else {
        diff
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
    editor: EditorKind,
    hashline: Option<std::sync::Arc<super::hashline::HashlineSession>>,
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
        session: std::sync::Arc<super::hashline::HashlineSession>,
    ) -> Self {
        self.hashline = Some(session);
        self
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
            let all_lines: Vec<&str> = content.lines().collect();
            if self.editor == EditorKind::Hashline {
                let mut clipped = Vec::new();
                let mut truncated = false;
                for (index, line) in all_lines.iter().copied().enumerate() {
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
                    results.push(super::hashline::format_grep_file(
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
            for (index, line) in all_lines.iter().copied().enumerate() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_paginates_whole_lines_and_long_utf8_lines() {
        let args = ReadArgs {
            path: "sample".into(),
            offset: None,
            limit: Some(1),
            byte_offset: None,
        };
        let output = read_text("alpha\nbeta", &args).unwrap();
        assert!(output.contains("1: alpha\n[Continue with Read offset=2.]"));
        assert!(output.contains(&content_version("alpha\nbeta")));
        let text = "界".repeat(super::super::MAX_INLINE_OUTPUT_BYTES);
        let mut restored = String::new();
        let mut byte_offset = 0;
        loop {
            let page = read_text(
                &text,
                &ReadArgs {
                    byte_offset: Some(byte_offset),
                    path: args.path.clone(),
                    ..args
                },
            )
            .unwrap();
            assert!(page.len() <= super::super::MAX_INLINE_OUTPUT_BYTES);
            let (_, code) = page.lines().nth(1).unwrap().split_once(": ").unwrap();
            restored.push_str(code);
            byte_offset += code.len();
            if byte_offset == text.len() {
                assert!(page.contains("[End of file]"));
                break;
            }
            assert!(page.contains(&format!("byte_offset={byte_offset}, limit=1")));
        }
        assert_eq!(restored, text);
        assert!(
            read_text(
                &text,
                &ReadArgs {
                    byte_offset: Some(1),
                    path: args.path.clone(),
                    ..args
                }
            )
            .is_err()
        );
        let content = format!("{}\n{}", "a".repeat(20000), "b".repeat(20000));
        let page = read_text(
            &content,
            &ReadArgs {
                limit: None,
                path: args.path.clone(),
                ..args
            },
        )
        .unwrap();
        assert!(page.contains("offset=2"));
        assert!(!page.contains("2: b"));
        assert!(read_text("", &args).unwrap().ends_with("[End of file]"));
        assert!(
            read_text(
                "a",
                &ReadArgs {
                    offset: Some(0),
                    path: args.path.clone(),
                    ..args
                }
            )
            .is_err()
        );
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
    async fn edit_preserves_crlf_and_requires_version_for_overwrite() {
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
            "gamma\r\ndelta\r\n"
        );
        assert!(
            tool.execute(r#"{"path":"sample","old_string":"","new_string":"reset"}"#)
                .await
                .unwrap_err()
                .contains("expected_version")
        );
        for replacement in ["reset\r\n", ""] {
            let version = content_version(&fs::read_to_string(root.join("sample")).unwrap());
            tool.execute(&serde_json::json!({"path":"sample", "old_string":"", "new_string":replacement, "expected_version":version}).to_string()).await.unwrap();
            assert_eq!(
                fs::read_to_string(root.join("sample")).unwrap(),
                replacement
            );
        }
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
        assert!(result.ends_with("1: hello\n[End of file]"));

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_hashline_emits_snapshot_header_and_line_numbers() {
        let root = std::env::temp_dir().join(format!("vva-hash-read-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "alpha\nbeta\nalpha\n").unwrap();
        let tool = ReadTool::new(&root).with_editor(crate::model::EditorKind::Hashline);
        let result = tool.execute(r#"{"path":"sample"}"#).await.unwrap();
        let header = result.lines().next().unwrap();
        assert!(header.starts_with("[sample#"), "{header}");
        assert_eq!(header.len(), "[sample#ABCD]".len(), "{header}");
        assert!(result.contains("1:alpha"));
        assert!(result.contains("2:beta"));
        assert!(result.contains("3:alpha"));
        assert!(!result.contains("1: "));
        assert!(!result.contains("[version:"));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn grep_hashline_emits_snapshot_header_and_line_numbers() {
        let root = std::env::temp_dir().join(format!("vva-hash-grep-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "alpha\nbeta\nalpha\n").unwrap();
        let tool = GrepTool::new(&root).with_editor(crate::model::EditorKind::Hashline);
        let result = tool.execute(r#"{"pattern":"alpha"}"#).await.unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert!(lines[0].starts_with("[sample#"), "{}", lines[0]);
        assert_eq!(lines[1], "1:alpha");
        assert_eq!(lines[2], "3:alpha");
        let numbered = GrepTool::new(&root)
            .execute(r#"{"pattern":"alpha"}"#)
            .await
            .unwrap();
        assert!(numbered.contains(":1: alpha"));
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
                "--- {0}\n+++ {0}\n@@ -1,3 +1,3 @@\n one\n-before\n+after\n three\n[version: {1}]",
                root.join("file.txt").display(),
                content_version("one\nafter\nthree\n")
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
        assert!(error.starts_with("unchanged:"));

        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn batch_edits_use_original_and_fail_without_writing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample");
        let original = "\u{feff}first\r\nsecond\r\nkeep\nlast";
        fs::write(&path, original).unwrap();
        let registry = crate::tools::ToolRegistry::with_editor(
            root.path(),
            None,
            crate::model::EditorKind::Replace,
        );
        let execute = |edits: serde_json::Value| {
            ToolCall::new(
                "batch",
                "Edit",
                serde_json::json!({"path":"sample", "edits":edits}).to_string(),
            )
        };
        let result = registry
            .execute_call(&execute(serde_json::json!([
                {"old_string":"first", "new_string":"second"},
                {"old_string":"second", "new_string":"third\nfourth"}
            ])))
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\u{feff}second\r\nthird\r\nfourth\r\nkeep\nlast"
        );
        for (edits, error) in [
            (
                serde_json::json!([{"old_string":"second", "new_string":"ok"},{"old_string":"missing", "new_string":"no"}]),
                "not_found",
            ),
            (
                serde_json::json!([{"old_string":"third\nfourth", "new_string":"x"},{"old_string":"fourth", "new_string":"y"}]),
                "overlap",
            ),
            (
                serde_json::json!([{"old_string":"keep", "new_string":"x", "extra":true}]),
                "Unexpected parameter",
            ),
        ] {
            let before = fs::read(&path).unwrap();
            let result = registry.execute_call(&execute(edits)).await;
            assert!(
                result.is_error && result.content.contains(error),
                "{}",
                result.content
            );
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn stale_version_rejects_edit_and_fresh_version_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample");
        fs::write(&path, "value = 1;\n").unwrap();
        let read = ReadTool::new(root.path())
            .execute(r#"{"path":"sample"}"#)
            .await
            .unwrap();
        let version = read
            .lines()
            .next()
            .unwrap()
            .strip_prefix("[version: ")
            .unwrap()
            .strip_suffix(']')
            .unwrap();
        fs::write(&path, "value = 1;\nexternal\n").unwrap();
        let edit = EditTool::new(root.path());
        let arguments = |version: &str| {
            serde_json::json!({"path":"sample", "expected_version":version, "old_string":"value = 1", "new_string":"value = 2"}).to_string()
        };
        assert!(
            edit.execute(&arguments(version))
                .await
                .unwrap_err()
                .starts_with("stale:")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "value = 1;\nexternal\n");
        let version = content_version(&fs::read_to_string(&path).unwrap());
        edit.execute(&arguments(&version)).await.unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "value = 2;\nexternal\n");
    }

    #[test]
    fn repeated_and_overlapping_matches_are_explicit() {
        let edit = |old: &str, all, count| Replacement {
            old_string: old.into(),
            new_string: "z".into(),
            replace_all: Some(all),
            expected_matches: count,
        };
        assert!(
            apply_replacements("x\nx", &[edit("x", false, None)])
                .unwrap_err()
                .contains("lines [1, 2]")
        );
        assert!(
            apply_replacements("aaa", &[edit("aa", false, None)])
                .unwrap_err()
                .starts_with("ambiguous:")
        );
        assert!(
            apply_replacements("aaa", &[edit("aa", true, None)])
                .unwrap_err()
                .starts_with("overlap:")
        );
        assert!(
            apply_replacements("x x", &[edit("x", true, Some(1))])
                .unwrap_err()
                .starts_with("match_count:")
        );
        assert_eq!(
            apply_replacements("x x", &[edit("x", true, Some(2))])
                .unwrap()
                .updated,
            "z z"
        );
    }

    #[test]
    fn staged_write_rechecks_content_and_cleans_up() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample");
        fs::write(&path, "external").unwrap();
        assert!(
            persist_edit(&path, "old", "new")
                .unwrap_err()
                .starts_with("stale:")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "external");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_preserves_symlink_target_and_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o751)).unwrap();
        symlink(&target, root.path().join("link")).unwrap();
        EditTool::new(root.path())
            .execute(r#"{"path":"link","old_string":"old","new_string":"new"}"#)
            .await
            .unwrap();
        assert!(
            fs::symlink_metadata(root.path().join("link"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o751
        );
    }
    #[test]
    fn diff_keeps_distant_edits_compact_and_reports_final_newline() {
        let original: String = (1..=1000).map(|line| format!("line {line}\n")).collect();
        let edits = [
            Replacement {
                old_string: "line 1\n".into(),
                new_string: "first\nextra\n".into(),
                replace_all: None,
                expected_matches: None,
            },
            Replacement {
                old_string: "line 1000\n".into(),
                new_string: "last\n".into(),
                replace_all: None,
                expected_matches: None,
            },
        ];
        let plan = apply_replacements(&original, &edits).unwrap();
        let diff = edit_diff(Path::new("sample"), &original, &plan);
        assert_eq!(
            diff.lines().filter(|line| line.starts_with("@@")).count(),
            2
        );
        assert!(diff.contains("@@ -1,4 +1,5 @@"), "{diff}");
        assert!(diff.contains("@@ -997,4 +998,4 @@"), "{diff}");
        assert!(diff.len() < 500);
        assert!(!diff.contains("line 500"));
        let diff = unified_diff(Path::new("sample"), "value", "value\n", 0, 0);
        assert!(
            diff.contains("-value\n\\ No newline at end of file\n+value"),
            "{diff}"
        );
    }

    #[test]
    fn inserted_lines_follow_local_line_endings() {
        let original = "head\r\ntarget\ntail\r\n";
        let edit = Replacement {
            old_string: "target".into(),
            new_string: "new\nline".into(),
            replace_all: None,
            expected_matches: None,
        };
        assert_eq!(
            apply_replacements(original, &[edit]).unwrap().updated,
            "head\r\nnew\nline\ntail\r\n"
        );
    }

    #[tokio::test]
    async fn concurrent_edits_with_same_version_apply_once() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample");
        fs::write(&path, "original").unwrap();
        let arguments = serde_json::json!({"path":"sample", "expected_version":content_version("original"), "old_string":"original", "new_string":"updated"}).to_string();
        let first = EditTool::new(root.path());
        let second = EditTool::new(root.path());
        let (a, b) = tokio::join!(first.execute(&arguments), second.execute(&arguments));
        assert_ne!(a.is_ok(), b.is_ok());
        let error = if let Err(error) = a {
            error
        } else {
            b.unwrap_err()
        };
        assert!(error.starts_with("stale:"));
        assert_eq!(fs::read_to_string(path).unwrap(), "updated");
    }
}
