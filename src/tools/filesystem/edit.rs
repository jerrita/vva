//! Edit 工具：精确替换引擎（规范化 / 冲突检测 / 行尾保持 / 原子落盘），
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::Tool;
use super::line_offsets;
use super::read::content_version;
use super::resolve_path;
use super::unified_diff;
use crate::tools::INLINE_OUTPUT_BUDGET;
use crate::tools::parse_args;

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
pub(super) struct EditArgs {
    pub(super) path: String,
    pub(super) expected_version: Option<String>,
    pub(super) edits: Option<Vec<Replacement>>,
    pub(super) old_string: Option<String>,
    pub(super) new_string: Option<String>,
    pub(super) replace_all: Option<bool>,
    pub(super) expected_matches: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Replacement {
    pub(super) old_string: String,
    pub(super) new_string: String,
    pub(super) replace_all: Option<bool>,
    pub(super) expected_matches: Option<usize>,
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

pub(super) fn edit_diff(path: &Path, original: &str, plan: &EditPlan) -> String {
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

pub(super) fn apply_replacements(raw: &str, edits: &[Replacement]) -> Result<EditPlan, String> {
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
        let args: EditArgs = parse_args(arguments, "Edit")?;
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
    let budget = INLINE_OUTPUT_BUDGET;
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
