//! omp hashline: file snapshot tags, original line numbers, PUT/CUT/REM/MV.

mod block;
mod parse;

use super::Tool;
use super::filesystem::{
    ChangedRange, EditPlan, edit_lock, format_edit_result, persist_edit, resolve_path,
};
use crate::tools::MAX_INLINE_OUTPUT_BYTES;
use async_trait::async_trait;
use parse::{FileOp, Op, Section, parse_patch};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct HashlineSession {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    files: HashMap<PathBuf, HashMap<String, FileSnap>>,
    named: HashMap<String, Vec<String>>,
}

#[derive(Clone)]
struct FileSnap {
    text: String,
    seen: HashSet<u32>,
}

impl HashlineSession {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, String> {
        self.inner
            .lock()
            .map_err(|_| "hashline snapshot lock poisoned".into())
    }

    pub fn record(
        &self,
        canonical: PathBuf,
        text: &str,
        seen: impl IntoIterator<Item = u32>,
    ) -> Result<String, String> {
        let tag = file_tag(text);
        let mut inner = self.lock()?;
        let snaps = inner.files.entry(canonical).or_default();
        let snap = snaps.entry(tag.clone()).or_insert_with(|| FileSnap {
            text: text.to_string(),
            seen: HashSet::new(),
        });
        if snap.text != text {
            snap.text = text.to_string();
            snap.seen.clear();
        }
        snap.seen.extend(seen);
        Ok(tag)
    }

    fn lookup(&self, canonical: &Path, tag: &str) -> Result<FileSnap, String> {
        let inner = self.lock()?;
        inner
            .files
            .get(canonical)
            .and_then(|snaps| snaps.get(tag))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "not_found: snapshot [{}#{}] is not registered in this active session (including after session reload); this does not indicate a disk change. Read or Grep the file and copy the header",
                    canonical.display(),
                    tag
                )
            })
    }

    fn publish_named(&self, named: HashMap<String, Vec<String>>) -> Result<(), String> {
        if named.is_empty() {
            return Ok(());
        }
        self.lock()?.named.extend(named);
        Ok(())
    }

    fn named(&self, name: &str) -> Result<Option<Vec<String>>, String> {
        Ok(self.lock()?.named.get(name).cloned())
    }
}

pub fn file_tag(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))[..4].to_ascii_uppercase()
}

pub fn format_read(
    path_label: &str,
    content: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    byte_offset: Option<usize>,
    session: Option<&HashlineSession>,
    canonical: &Path,
) -> Result<String, String> {
    let offset = offset.unwrap_or(1);
    if offset == 0 {
        return Err("offset must be a positive, 1-indexed line number".into());
    }
    if byte_offset.is_some() && limit != Some(1) {
        return Err("byte_offset requires limit=1".into());
    }
    let file = LineFile::parse(content);
    let tag = file_tag(content);
    let budget = MAX_INLINE_OUTPUT_BYTES - 256;
    let mut output = format!("[{path_label}#{tag}]\n");
    let mut seen = Vec::new();
    let mut shown = 0;
    let mut index = offset.saturating_sub(1);
    while shown < limit.unwrap_or(usize::MAX) {
        if index >= file.lines.len() {
            if byte_offset.is_some() {
                return Err("byte_offset refers to a line past EOF".into());
            }
            output.push_str("[End of file]");
            record_read(session, canonical, content, seen)?;
            return Ok(output);
        }
        let line = &file.lines[index];
        let skip = byte_offset.unwrap_or(0);
        if skip > line.len() || !line.is_char_boundary(skip) {
            return Err(format!(
                "byte_offset must be a UTF-8 boundary within line {} ({} bytes)",
                index + 1,
                line.len()
            ));
        }
        let prefix = format!("{}:", index + 1);
        let text = &line[skip..];
        let remaining = budget.saturating_sub(output.len());
        if prefix.len() + text.len() + 1 > remaining {
            if shown > 0 {
                output.push_str(&format!("[Continue with Read offset={}.]", index + 1));
                record_read(session, canonical, content, seen)?;
                return Ok(output);
            }
            let mut cut = remaining.saturating_sub(prefix.len() + 1).min(text.len());
            while !text.is_char_boundary(cut) {
                cut = cut.saturating_sub(1);
            }
            output.push_str(&prefix);
            output.push_str(&text[..cut]);
            output.push_str(&format!(
                "\n[Partial line; continue with Read offset={}, byte_offset={}, limit=1.]",
                index + 1,
                skip + cut
            ));
            record_read(session, canonical, content, seen)?;
            return Ok(output);
        }
        output.push_str(&prefix);
        output.push_str(text);
        output.push('\n');
        if skip == 0 {
            seen.push((index + 1) as u32);
        }
        shown += 1;
        index += 1;
    }
    if index < file.lines.len() {
        output.push_str(&format!("[Continue with Read offset={}.]", index + 1));
    } else {
        output.push_str("[End of file]");
    }
    record_read(session, canonical, content, seen)?;
    Ok(output)
}

fn record_read(
    session: Option<&HashlineSession>,
    canonical: &Path,
    content: &str,
    seen: Vec<u32>,
) -> Result<(), String> {
    if let Some(session) = session {
        session.record(canonical.to_path_buf(), content, seen)?;
    }
    Ok(())
}

pub fn format_grep_file(
    path_label: &str,
    content: &str,
    matches: &[(usize, &str)],
    session: Option<&HashlineSession>,
    canonical: &Path,
) -> Result<String, String> {
    if matches.is_empty() {
        return Ok(String::new());
    }
    let tag = file_tag(content);
    let mut output = format!("[{path_label}#{tag}]\n");
    let mut seen = Vec::new();
    for (index, line) in matches {
        output.push_str(&format!("{}:{line}\n", index + 1));
        seen.push((*index + 1) as u32);
    }
    if let Some(session) = session {
        session.record(canonical.to_path_buf(), content, seen)?;
    }
    output.pop();
    Ok(output)
}

pub struct HashlineEditTool {
    working_dir: PathBuf,
    session: Option<Arc<HashlineSession>>,
}

impl HashlineEditTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            session: None,
        }
    }

    pub fn with_session(mut self, session: Arc<HashlineSession>) -> Self {
        self.session = Some(session);
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HashlineArgs {
    input: String,
}

#[async_trait]
impl Tool for HashlineEditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        r#"Line-anchored patch. Copy [PATH#TAG] from Read, Grep, or the latest Edit preview; numbers are original snapshot lines, not shifted by earlier hunks. New files use Write.

{"input":"[src/example.rs#1A2B]\nPUT 4.=4:\n+const value = 2;\n"}

PUT N.=M: replace inclusive N–M with + body; every line in the range is removed, so include any retained lines in the body. Prefer <N or >N for insertion.
PUT N*: replace a heuristically detected block (bracket counting, indentation, or Markdown heading; no language parser). Use explicit ranges when strings, comments, or JSX affect boundaries.
PUT <N: insert before N (`PUT <1:` = file head). PUT >N: after N. PUT >$: append. PUT >N*: after block N.
CUT N.=M / CUT N* deletes and optionally captures @name. PUT <N @name / PUT >N @name / PUT N.=M @name pastes (range/block paste needs a named register; gap paste may be anonymous).
REM deletes the section file. MV DEST moves after prior edits in that section.
Body rows start with +; + alone writes a blank line. Literal leading - or +: +-text / ++text. Never send -old or context rows.
After Edit, use its new [PATH#TAG] and numbered preview lines; Read any other target ranges. All hunks in one request use the original snapshot's numbers. Read/Grep and numbered Edit previews accumulate displayed lines for the same TAG. Elided and partial lines are unseen. Active-session snapshots survive submissions; after session reload, Read/Grep again."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "One or more [PATH#TAG] sections of PUT/CUT/REM/MV. Optional *** Begin Patch / *** End Patch wrapper."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: HashlineArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments for Edit: {error}"))?;
        let sections = parse_patch(&args.input)?;
        let working_dir = self.working_dir.clone();
        let session = self
            .session
            .clone()
            .ok_or_else(|| "hashline session is missing; Read the file before Edit".to_string())?;
        tokio::task::spawn_blocking(move || apply_sections(&working_dir, &session, sections))
            .await
            .map_err(|e| format!("Edit task failed: {e}"))?
    }
}

struct LineFile {
    lines: Vec<String>,
    ending: &'static str,
    trailing_newline: bool,
}

impl LineFile {
    fn parse(raw: &str) -> Self {
        let ending = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_newline = raw.ends_with('\n');
        let normalized = raw.replace("\r\n", "\n");
        let lines = if raw.is_empty() {
            Vec::new()
        } else {
            let mut lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();
            if trailing_newline {
                lines.pop();
            }
            lines
        };
        Self {
            lines,
            ending,
            trailing_newline,
        }
    }

    fn render(&self) -> String {
        let mut out = self.lines.join(self.ending);
        if self.trailing_newline && !self.lines.is_empty() {
            out.push_str(self.ending);
        }
        out
    }
}

struct Span {
    start: usize,
    end: usize,
    replacement: Vec<String>,
    op_index: usize,
}

fn apply_sections(
    working_dir: &Path,
    session: &HashlineSession,
    sections: Vec<Section>,
) -> Result<String, String> {
    let grouped = group_sections(working_dir, sections)?;
    let mut prepared = Vec::new();
    let mut batch_named: HashMap<String, Vec<String>> = HashMap::new();
    let mut anonymous: Option<Vec<String>> = None;
    for group in grouped {
        prepared.push(
            prepare_group(session, group, &mut batch_named, &mut anonymous)
                .map_err(|error| format!("{error}; preflight failed, no files were changed"))?,
        );
    }
    let mut reports = Vec::new();
    for prepared in prepared {
        reports.push(commit_group(working_dir, session, prepared)?);
    }
    session.publish_named(batch_named)?;
    Ok(reports.join("\n\n"))
}

struct Group {
    label: String,
    tag: String,
    canonical: PathBuf,
    ops: Vec<Op>,
    file_op: Option<FileOp>,
}

fn group_sections(working_dir: &Path, sections: Vec<Section>) -> Result<Vec<Group>, String> {
    let mut order: Vec<Group> = Vec::new();
    let mut map: HashMap<PathBuf, usize> = HashMap::new();
    for section in sections {
        let resolved = resolve_path(working_dir, &section.path);
        let canonical = fs::canonicalize(&resolved).unwrap_or(resolved);
        if let Some(&index) = map.get(&canonical) {
            let existing = &mut order[index];
            if existing.tag != section.tag {
                return Err(format!(
                    "invalid_arguments: {} has mixed snapshot tags {} and {}",
                    section.path, existing.tag, section.tag
                ));
            }
            existing.ops.extend(section.ops);
            if section.file_op.is_some() {
                existing.file_op = section.file_op;
            }
        } else {
            map.insert(canonical.clone(), order.len());
            order.push(Group {
                label: section.path,
                tag: section.tag,
                canonical,
                ops: section.ops,
                file_op: section.file_op,
            });
        }
    }
    Ok(order)
}

struct Prepared {
    label: String,
    canonical: PathBuf,
    original: String,
    updated: String,
    changes: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
    file_op: Option<FileOp>,
}

fn prepare_group(
    session: &HashlineSession,
    group: Group,
    batch_named: &mut HashMap<String, Vec<String>>,
    anonymous: &mut Option<Vec<String>>,
) -> Result<Prepared, String> {
    let original = if group.canonical.exists() {
        let raw = fs::read_to_string(&group.canonical)
            .map_err(|e| format!("Failed to read {}: {e}", group.canonical.display()))?;
        if raw.contains('\0') {
            return Err("Unsupported binary file".into());
        }
        raw
    } else {
        return Err(format!("Failed to resolve {}", group.canonical.display()));
    };
    let snap = session.lookup(&group.canonical, &group.tag)?;
    if snap.text != original {
        return Err(format!(
            "stale: [{}#{}] no longer matches disk (now [{}#{}]); Read and retry",
            group.label,
            group.tag,
            group.label,
            file_tag(&original)
        ));
    }
    let file = LineFile::parse(&original);
    let refs: Vec<&str> = file.lines.iter().map(String::as_str).collect();
    let mut spans = Vec::new();
    for (op_index, op) in group.ops.iter().enumerate() {
        if let Some(captured) = capture(op, &refs, &group.label)? {
            write_register(captured.register, captured.lines, batch_named, anonymous)?;
        }
        let span = op_span(
            op,
            &refs,
            &group.label,
            &snap.seen,
            &Registers {
                session,
                named: batch_named,
                anonymous,
            },
            op_index,
        )
        .map_err(|error| {
            format!(
                "[{}#{}] op {}: {error}",
                group.label,
                group.tag,
                op_index + 1
            )
        })?;
        if let Some(span) = span {
            spans.push(span);
        }
    }
    spans.sort_by_key(|span| (span.start, span.end));
    for pair in spans.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(format!(
                "overlap: ops {} and {} touch overlapping original lines; merge them",
                pair[0].op_index + 1,
                pair[1].op_index + 1
            ));
        }
    }
    let mut updated_lines = Vec::new();
    let mut changes = Vec::new();
    let mut cursor = 0;
    for span in spans {
        updated_lines.extend(file.lines[cursor..span.start].iter().cloned());
        let new_start = updated_lines.len();
        updated_lines.extend(span.replacement);
        changes.push((span.start..span.end, new_start..updated_lines.len()));
        cursor = span.end;
    }
    updated_lines.extend(file.lines[cursor..].iter().cloned());
    let mut updated = file;
    updated.lines = updated_lines;
    if updated.lines.is_empty() {
        updated.trailing_newline = false;
    }
    Ok(Prepared {
        label: group.label,
        canonical: group.canonical,
        original,
        updated: updated.render(),
        changes,
        file_op: group.file_op,
    })
}

struct Capture {
    register: Option<String>,
    lines: Vec<String>,
}

fn capture(op: &Op, lines: &[&str], path: &str) -> Result<Option<Capture>, String> {
    let (start, end, register) = match op {
        Op::Cut {
            start,
            end,
            register,
        } => (*start, *end, register.clone()),
        Op::CutBlock { line, register } => {
            let (start, end) = block::resolve_block(lines, *line, path)?;
            (start, end, register.clone())
        }
        _ => return Ok(None),
    };
    let (lo, hi) = inclusive_range(start, end, lines.len())?;
    Ok(Some(Capture {
        register,
        lines: lines[lo..hi]
            .iter()
            .map(|line| (*line).to_string())
            .collect(),
    }))
}

fn write_register(
    register: Option<String>,
    lines: Vec<String>,
    named: &mut HashMap<String, Vec<String>>,
    anonymous: &mut Option<Vec<String>>,
) -> Result<(), String> {
    match register {
        Some(name) => {
            named.insert(name, lines);
        }
        None => *anonymous = Some(lines),
    }
    Ok(())
}

fn read_register(
    register: Option<&str>,
    session: &HashlineSession,
    named: &HashMap<String, Vec<String>>,
    anonymous: &Option<Vec<String>>,
) -> Result<Vec<String>, String> {
    match register {
        Some(name) => named
            .get(name)
            .cloned()
            .or_else(|| session.named(name).ok().flatten())
            .ok_or_else(|| format!("not_found: register @{name} is empty; CUT into it first")),
        None => anonymous.clone().ok_or_else(|| {
            "not_found: anonymous register is empty; CUT without @name first".into()
        }),
    }
}

struct Registers<'a> {
    session: &'a HashlineSession,
    named: &'a HashMap<String, Vec<String>>,
    anonymous: &'a Option<Vec<String>>,
}

fn op_span(
    op: &Op,
    lines: &[&str],
    path: &str,
    seen: &HashSet<u32>,
    registers: &Registers<'_>,
    op_index: usize,
) -> Result<Option<Span>, String> {
    let require = |line: u32| require_seen(seen, line);
    let span = match op {
        Op::Replace { start, end, body } => {
            require_range(seen, *start, *end)?;
            let (lo, hi) = inclusive_range(*start, *end, lines.len())?;
            Span {
                start: lo,
                end: hi,
                replacement: body.clone(),
                op_index,
            }
        }
        Op::ReplaceBlock { line, body } => {
            let (start, end) = block::resolve_block(lines, *line, path)?;
            require_range(seen, start, end)?;
            let (lo, hi) = inclusive_range(start, end, lines.len())?;
            Span {
                start: lo,
                end: hi,
                replacement: body.clone(),
                op_index,
            }
        }
        Op::InsertBefore { line, body } => {
            require(*line)?;
            let at = line_index(*line, lines.len())?;
            Span {
                start: at,
                end: at,
                replacement: body.clone(),
                op_index,
            }
        }
        Op::InsertAfter { line, body } => {
            require(*line)?;
            let at = line_index(*line, lines.len())? + 1;
            Span {
                start: at,
                end: at,
                replacement: body.clone(),
                op_index,
            }
        }
        Op::InsertAfterBlock { line, body } => {
            let (start, end) = block::resolve_block(lines, *line, path)?;
            require_range(seen, start, end)?;
            let at = line_index(end, lines.len())? + 1;
            Span {
                start: at,
                end: at,
                replacement: body.clone(),
                op_index,
            }
        }
        Op::Append { body } => {
            if let Some(last) = lines.len().checked_sub(1) {
                require((last + 1) as u32)?;
            }
            let at = lines.len();
            Span {
                start: at,
                end: at,
                replacement: body.clone(),
                op_index,
            }
        }
        Op::InsertBof { body } => {
            if !lines.is_empty() {
                require(1)?;
            }
            Span {
                start: 0,
                end: 0,
                replacement: body.clone(),
                op_index,
            }
        }
        Op::Cut { start, end, .. } => {
            require_range(seen, *start, *end)?;
            let (lo, hi) = inclusive_range(*start, *end, lines.len())?;
            Span {
                start: lo,
                end: hi,
                replacement: Vec::new(),
                op_index,
            }
        }
        Op::CutBlock { line, .. } => {
            let (start, end) = block::resolve_block(lines, *line, path)?;
            require_range(seen, start, end)?;
            let (lo, hi) = inclusive_range(start, end, lines.len())?;
            Span {
                start: lo,
                end: hi,
                replacement: Vec::new(),
                op_index,
            }
        }
        Op::PasteBefore { line, register } => {
            require(*line)?;
            let at = line_index(*line, lines.len())?;
            Span {
                start: at,
                end: at,
                replacement: read_register(
                    register.as_deref(),
                    registers.session,
                    registers.named,
                    registers.anonymous,
                )?,
                op_index,
            }
        }
        Op::PasteAfter { line, register } => {
            require(*line)?;
            let at = line_index(*line, lines.len())? + 1;
            Span {
                start: at,
                end: at,
                replacement: read_register(
                    register.as_deref(),
                    registers.session,
                    registers.named,
                    registers.anonymous,
                )?,
                op_index,
            }
        }
        Op::PasteEof { register } => {
            if let Some(last) = lines.len().checked_sub(1) {
                require((last + 1) as u32)?;
            }
            let at = lines.len();
            Span {
                start: at,
                end: at,
                replacement: read_register(
                    register.as_deref(),
                    registers.session,
                    registers.named,
                    registers.anonymous,
                )?,
                op_index,
            }
        }
        Op::PasteBof { register } => {
            if !lines.is_empty() {
                require(1)?;
            }
            Span {
                start: 0,
                end: 0,
                replacement: read_register(
                    register.as_deref(),
                    registers.session,
                    registers.named,
                    registers.anonymous,
                )?,
                op_index,
            }
        }
        Op::PasteRange {
            start,
            end,
            register,
        } => {
            require_range(seen, *start, *end)?;
            let (lo, hi) = inclusive_range(*start, *end, lines.len())?;
            Span {
                start: lo,
                end: hi,
                replacement: read_register(
                    Some(register),
                    registers.session,
                    registers.named,
                    registers.anonymous,
                )?,
                op_index,
            }
        }
        Op::PasteBlock { line, register } => {
            let (start, end) = block::resolve_block(lines, *line, path)?;
            require_range(seen, start, end)?;
            let (lo, hi) = inclusive_range(start, end, lines.len())?;
            Span {
                start: lo,
                end: hi,
                replacement: read_register(
                    Some(register),
                    registers.session,
                    registers.named,
                    registers.anonymous,
                )?,
                op_index,
            }
        }
        Op::PasteAfterBlock { line, register } => {
            let (start, end) = block::resolve_block(lines, *line, path)?;
            require_range(seen, start, end)?;
            let at = line_index(end, lines.len())? + 1;
            Span {
                start: at,
                end: at,
                replacement: read_register(
                    register.as_deref(),
                    registers.session,
                    registers.named,
                    registers.anonymous,
                )?,
                op_index,
            }
        }
    };
    Ok(Some(span))
}

fn require_seen(seen: &HashSet<u32>, line: u32) -> Result<(), String> {
    require_range(seen, line, line)
}

fn require_range(seen: &HashSet<u32>, start: u32, end: u32) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut range_start = None;
    for line in start..=end {
        if !seen.contains(&line) {
            range_start.get_or_insert(line);
        } else if let Some(first) = range_start.take() {
            missing.push(format!("{first}..={}", line - 1));
        }
    }
    if let Some(first) = range_start {
        missing.push(format!("{first}..={end}"));
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unseen: missing displayed lines {}; Read these ranges first. Displayed ranges accumulate for the same snapshot TAG",
            missing.join(", ")
        ))
    }
}

fn inclusive_range(start: u32, end: u32, len: usize) -> Result<(usize, usize), String> {
    if start == 0 || end < start {
        return Err("invalid_arguments: range must be 1-indexed and ordered".into());
    }
    let lo = (start as usize) - 1;
    let hi = end as usize;
    if hi > len {
        return Err(format!(
            "not_found: line {end} does not exist (file has {len} lines)"
        ));
    }
    Ok((lo, hi))
}

fn line_index(line: u32, len: usize) -> Result<usize, String> {
    if line == 0 {
        return Err("invalid_arguments: line numbers are 1-indexed".into());
    }
    let index = (line as usize) - 1;
    if index < len {
        Ok(index)
    } else {
        Err(format!(
            "not_found: line {line} does not exist (file has {len} lines)"
        ))
    }
}

fn commit_group(
    working_dir: &Path,
    session: &HashlineSession,
    prepared: Prepared,
) -> Result<String, String> {
    match prepared.file_op {
        Some(FileOp::Rem) => {
            let lock = edit_lock(&prepared.canonical)?;
            let _guard = lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            fs::remove_file(&prepared.canonical)
                .map_err(|e| format!("Failed to delete {}: {e}", prepared.canonical.display()))?;
            Ok(format!("Deleted file {}", prepared.label))
        }
        Some(FileOp::Move { dest }) => {
            let dest_path = resolve_path(working_dir, &dest);
            if dest_path.exists() {
                return Err(format!(
                    "Failed to move {} -> {}: destination exists",
                    prepared.label, dest
                ));
            }
            let src_lock = edit_lock(&prepared.canonical)?;
            let dest_lock = edit_lock(&dest_path)?;
            let _g1 = src_lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            let _g2 = dest_lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            write_new_file(&dest_path, &prepared.updated)?;
            fs::remove_file(&prepared.canonical)
                .map_err(|e| format!("Failed to remove {}: {e}", prepared.canonical.display()))?;
            let dest_canonical = fs::canonicalize(&dest_path).unwrap_or(dest_path.clone());
            report_update(
                session,
                dest,
                dest_canonical,
                &prepared.original,
                &prepared.updated,
                prepared.changes,
            )
        }
        None => {
            if prepared.updated == prepared.original {
                return Err("unchanged: edits produce no file changes".into());
            }
            let lock = edit_lock(&prepared.canonical)?;
            let _guard = lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            persist_edit(&prepared.canonical, &prepared.original, &prepared.updated)?;
            report_update(
                session,
                prepared.label,
                prepared.canonical,
                &prepared.original,
                &prepared.updated,
                prepared.changes,
            )
        }
    }
}

fn write_new_file(path: &Path, contents: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Failed to create file {}: {error}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("Failed to write file {}: {error}", path.display()))?;
    Ok(())
}

fn report_update(
    session: &HashlineSession,
    label: impl Into<String>,
    canonical: PathBuf,
    original: &str,
    updated: &str,
    changes: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
) -> Result<String, String> {
    let label = label.into();
    let old_offsets = line_offsets(original);
    let new_offsets = line_offsets(updated);
    let preview = numbered_preview(updated, &changes);
    let plan = EditPlan {
        updated: updated.to_string(),
        changes: changes
            .into_iter()
            .map(|(old, new)| ChangedRange {
                old: old_offsets[old.start]..old_offsets[old.end],
                new: new_offsets[new.start.min(new_offsets.len() - 1)]
                    ..new_offsets[new.end.min(new_offsets.len() - 1)],
            })
            .collect(),
    };
    let diff = format_edit_result(&canonical, original, &plan, false);
    let seen: Vec<u32> = preview.iter().map(|(n, _)| *n).collect();
    let tag = session.record(canonical, updated, seen)?;
    let mut out = format!("[{label}#{tag}]\n{diff}");
    if !preview.is_empty() {
        out.push_str("\nCurrent snapshot preview (only numbered lines are registered):\n");
        let mut previous = 0;
        for (number, line) in preview {
            if number > previous + 1 {
                out.push_str("[Unshown lines; Read before editing]\n");
            }
            out.push_str(&format!("{number}:{line}\n"));
            previous = number;
        }
        if (previous as usize) < new_offsets.len() - 1 {
            out.push_str("[Unshown lines; Read before editing]\n");
        }
        out.pop();
    }
    Ok(out)
}

fn line_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for line in text.split_inclusive('\n') {
        offsets.push(offsets.last().unwrap() + line.len());
    }
    offsets
}

fn numbered_preview(
    text: &str,
    changes: &[(std::ops::Range<usize>, std::ops::Range<usize>)],
) -> Vec<(u32, String)> {
    let file = LineFile::parse(text);
    let total = file.lines.len();
    let mut shown = std::collections::BTreeSet::new();
    for (_, range) in changes {
        let start = range.start.saturating_sub(3);
        let end = range.end.saturating_add(3).min(total);
        if end - start <= 16 {
            shown.extend(start..end);
        } else {
            shown.extend(start..start + 8);
            shown.extend(end - 8..end);
        }
    }
    file.lines
        .into_iter()
        .enumerate()
        .filter(|(index, _)| shown.contains(index))
        .map(|(index, line)| ((index + 1) as u32, line))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EditorKind;
    use crate::tools::Tool;
    use crate::tools::filesystem::ReadTool;

    fn input(body: &str) -> String {
        serde_json::json!({ "input": body }).to_string()
    }

    #[tokio::test]
    async fn rebuilt_registry_retains_reads_and_reports_missing_ranges() {
        use crate::tools::ToolRegistry;
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("sample"), "a\nb\nc\nd\ne\n").unwrap();
        let session = HashlineSession::new();
        let registry = ToolRegistry::with_editor_session(
            root.path(),
            None,
            EditorKind::Hashline,
            session.clone(),
        );
        registry
            .get("Read")
            .unwrap()
            .execute(r#"{"path":"sample","offset":1,"limit":1}"#)
            .await
            .unwrap();
        drop(registry);
        let registry =
            ToolRegistry::with_editor_session(root.path(), None, EditorKind::Hashline, session);
        let shown = registry
            .get("Read")
            .unwrap()
            .execute(r#"{"path":"sample","offset":3,"limit":1}"#)
            .await
            .unwrap();
        let header = shown.lines().next().unwrap();
        let edit = registry.get("Edit").unwrap();
        let error = edit
            .execute(&input(&format!("{header}\nPUT 1.=5:\n+x\n")))
            .await
            .unwrap_err();
        assert!(error.contains("op 1"), "{error}");
        assert!(error.contains("2..=2, 4..=5"), "{error}");
        assert!(error.contains("no files were changed"), "{error}");
        assert_eq!(
            fs::read_to_string(root.path().join("sample")).unwrap(),
            "a\nb\nc\nd\ne\n"
        );
        edit.execute(&input(&format!("{header}\nPUT 1.=1:\n+A\nPUT 3.=3:\n+C\n")))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn distant_hunks_have_local_diff_and_reusable_shifted_preview() {
        let root = tempfile::tempdir().unwrap();
        let text: String = (1..=160).map(|n| format!("row {n}\n")).collect();
        let path = root.path().join("sample");
        fs::write(&path, &text).unwrap();
        let session = HashlineSession::new();
        let tag = session
            .record(fs::canonicalize(&path).unwrap(), &text, 1..=160)
            .unwrap();
        let edit = HashlineEditTool::new(root.path()).with_session(session.clone());
        let output = edit
            .execute(&input(&format!(
                "[sample#{tag}]\nPUT 12.=12:\n+first\n+extra\nPUT 140.=140:\n+second\n"
            )))
            .await
            .unwrap();
        assert_eq!(output.matches("@@ -").count(), 2, "{output}");
        assert!(!output.contains("row 80"), "{output}");
        assert!(output.contains("141:second"), "{output}");
        let current = fs::read_to_string(&path).unwrap();
        let snap = session
            .lookup(&fs::canonicalize(&path).unwrap(), &file_tag(&current))
            .unwrap();
        assert!(snap.seen.contains(&141));
        assert!(!snap.seen.contains(&80));
        let header = output.lines().next().unwrap();
        edit.execute(&input(&format!("{header}\nPUT 141.=141:\n+final\n")))
            .await
            .unwrap();
    }

    #[test]
    fn previews_cover_deletions_and_elide_large_replacements() {
        let text: String = (1..=100).map(|n| format!("row {n}\n")).collect();
        let preview = numbered_preview(&text, &[(10..90, 10..90)]);
        assert_eq!(preview.len(), 16);
        assert!(!preview.iter().any(|(n, _)| *n == 50));
        let preview = numbered_preview(&text, &[(100..110, 100..100)]);
        assert_eq!(
            preview.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![98, 99, 100]
        );
        let session = HashlineSession::new();
        let report = report_update(
            &session,
            "sample",
            PathBuf::from("sample"),
            "last",
            "",
            vec![(0..1, 0..1)],
        )
        .unwrap();
        assert!(report.contains("-last"));
    }

    #[test]
    fn parse_put_cut_and_header() {
        let sections = parse_patch(
            "[greet.py#A1B2]\nPUT 1.=3:\n+def greet(name):\n+    print(name)\nCUT 4.=4\n",
        )
        .unwrap();
        assert_eq!(sections[0].path, "greet.py");
        assert_eq!(sections[0].tag, "A1B2");
        assert_eq!(sections[0].ops.len(), 2);
    }

    #[test]
    fn parse_rejects_range_without_dot_equals() {
        let err = parse_patch("[a.rs#ABCD]\nPUT 1-2:\n+x\n").unwrap_err();
        assert!(err.contains("N.=M"), "{err}");
    }

    #[tokio::test]
    async fn read_then_put_replace_and_insert() {
        let root = std::env::temp_dir().join(format!("vva-hl-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "alpha\nbeta\ngamma\n").unwrap();
        let session = HashlineSession::new();
        let read = ReadTool::new(&root)
            .with_editor(EditorKind::Hashline)
            .with_hashline(session.clone());
        let shown = read.execute(r#"{"path":"sample"}"#).await.unwrap();
        assert!(shown.starts_with("[sample#"));
        assert!(shown.contains("1:alpha"));
        let tag = shown
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("[sample#")
            .trim_end_matches(']');
        let edit = HashlineEditTool::new(&root).with_session(session);
        edit.execute(&input(&format!(
            "[sample#{tag}]\nPUT 1.=1:\n+ALPHA\nPUT >2:\n+mid\nPUT 3.=3:\n+GAMMA\n+TAIL\n"
        )))
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sample")).unwrap(),
            "ALPHA\nbeta\nmid\nGAMMA\nTAIL\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn unseen_and_stale_fail() {
        let root = std::env::temp_dir().join(format!("vva-hl-fail-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "a\nb\nc\n").unwrap();
        let session = HashlineSession::new();
        let read = ReadTool::new(&root)
            .with_editor(EditorKind::Hashline)
            .with_hashline(session.clone());
        let shown = read
            .execute(r#"{"path":"sample","offset":1,"limit":1}"#)
            .await
            .unwrap();
        let tag = shown
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("[sample#")
            .trim_end_matches(']');
        let edit = HashlineEditTool::new(&root).with_session(session.clone());
        let unseen = edit
            .execute(&input(&format!("[sample#{tag}]\nPUT 3.=3:\n+C\n")))
            .await
            .unwrap_err();
        assert!(unseen.contains("unseen"), "{unseen}");
        fs::write(root.join("sample"), "a\nb\nc\nchanged\n").unwrap();
        let stale = edit
            .execute(&input(&format!("[sample#{tag}]\nPUT 1.=1:\n+A\n")))
            .await
            .unwrap_err();
        assert!(stale.contains("stale"), "{stale}");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn put_star_replaces_brace_block() {
        let root = std::env::temp_dir().join(format!("vva-hl-block-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "fn greet() {\n    hi()\n}\ncall();\n").unwrap();
        let session = HashlineSession::new();
        let read = ReadTool::new(&root)
            .with_editor(EditorKind::Hashline)
            .with_hashline(session.clone());
        let shown = read.execute(r#"{"path":"sample"}"#).await.unwrap();
        let tag = shown
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("[sample#")
            .trim_end_matches(']');
        let edit = HashlineEditTool::new(&root).with_session(session);
        edit.execute(&input(&format!(
            "[sample#{tag}]\nPUT 1*:\n+fn greet() {{\n+    hello()\n+}}\n"
        )))
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sample")).unwrap(),
            "fn greet() {\n    hello()\n}\ncall();\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cut_paste_named_register() {
        let root = std::env::temp_dir().join(format!("vva-hl-clip-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a"), "one\ntwo\nthree\n").unwrap();
        fs::write(root.join("b"), "keep\n").unwrap();
        let session = HashlineSession::new();
        let read = ReadTool::new(&root)
            .with_editor(EditorKind::Hashline)
            .with_hashline(session.clone());
        let a = read.execute(r#"{"path":"a"}"#).await.unwrap();
        let b = read.execute(r#"{"path":"b"}"#).await.unwrap();
        let tag_a = a
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("[a#")
            .trim_end_matches(']');
        let tag_b = b
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("[b#")
            .trim_end_matches(']');
        let edit = HashlineEditTool::new(&root).with_session(session);
        edit.execute(&input(&format!(
            "[a#{tag_a}]\nCUT 1.=2 @head\n[b#{tag_b}]\nPUT <1 @head\n"
        )))
        .await
        .unwrap();
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "three\n");
        assert_eq!(
            fs::read_to_string(root.join("b")).unwrap(),
            "one\ntwo\nkeep\n"
        );
        let _ = fs::remove_dir_all(root);
    }
}
