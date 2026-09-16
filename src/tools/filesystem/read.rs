//! Read 工具：文本分页 / 媒体读取 / hashline 快照。

use super::parse_args;
use super::{Tool, resolve_path};
use crate::config::resolver::InputCapabilities;
use crate::model::{EditorKind, MediaContent, MediaKind, ToolCall, ToolResult};
use crate::tools::INLINE_OUTPUT_BUDGET;
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct ReadTool {
    working_dir: PathBuf,
    inputs: InputCapabilities,
    editor: EditorKind,
    hashline: Option<std::sync::Arc<crate::tools::hashline::HashlineSession>>,
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
        session: std::sync::Arc<crate::tools::hashline::HashlineSession>,
    ) -> Self {
        self.hashline = Some(session);
        self
    }

    async fn read(&self, arguments: &str) -> Result<(String, Vec<MediaContent>), String> {
        let args: ReadArgs = parse_args(arguments, "Read")?;
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
                crate::tools::hashline::format_read(
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

pub(super) fn content_version(content: &str) -> String {
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
    let budget = INLINE_OUTPUT_BUDGET;
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
pub(super) fn read_text(content: &str, args: &ReadArgs) -> Result<String, String> {
    format_read_text(content, args, EditorKind::Replace)
}

#[derive(Deserialize)]
pub(super) struct ReadArgs {
    pub(super) path: String,
    pub(super) offset: Option<usize>,
    pub(super) limit: Option<usize>,
    pub(super) byte_offset: Option<usize>,
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
