//! 补丁应用：分组、预检、行号校验、寄存器与落盘提交。

use super::block;
use super::parse::{FileOp, Op, Section};
use super::session::{HashlineSession, file_tag};
use super::snapshot::report_update;
use crate::tools::filesystem::{edit_lock, persist_edit, resolve_path, write_new_file};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) struct LineFile {
    pub(super) lines: Vec<String>,
    ending: &'static str,
    trailing_newline: bool,
}

impl LineFile {
    pub(super) fn parse(raw: &str) -> Self {
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

    pub(super) fn render(&self) -> String {
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

pub(super) fn apply_sections(
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
    changes: Vec<Change>,
    file_op: Option<FileOp>,
    base_seen: HashSet<u32>,
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
            write_register(captured.register, captured.lines, batch_named, anonymous);
        }
        spans.push(
            op_span(
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
            })?,
        );
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
        base_seen: snap.seen,
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
) {
    match register {
        Some(name) => {
            named.insert(name, lines);
        }
        None => *anonymous = Some(lines),
    }
}

struct Registers<'a> {
    session: &'a HashlineSession,
    named: &'a HashMap<String, Vec<String>>,
    anonymous: &'a Option<Vec<String>>,
}

impl Registers<'_> {
    fn read(&self, register: Option<&str>) -> Result<Vec<String>, String> {
        match register {
            Some(name) => self
                .named
                .get(name)
                .cloned()
                .or_else(|| self.session.named(name).ok().flatten())
                .ok_or_else(|| format!("not_found: register @{name} is empty; CUT into it first")),
            None => self.anonymous.clone().ok_or_else(|| {
                "not_found: anonymous register is empty; CUT without @name first".into()
            }),
        }
    }
}

fn op_span(
    op: &Op,
    lines: &[&str],
    path: &str,
    seen: &HashSet<u32>,
    registers: &Registers<'_>,
    op_index: usize,
) -> Result<Span, String> {
    let require = |line: u32| require_seen(seen, line);
    let (start, end, replacement) = match op {
        Op::Replace { start, end, body } => {
            require_range(seen, *start, *end)?;
            let (lo, hi) = inclusive_range(*start, *end, lines.len())?;
            (lo, hi, body.clone())
        }
        Op::ReplaceBlock { line, body } => {
            let (lo, hi) = block_span(lines, *line, path, Some(seen))?;
            (lo, hi, body.clone())
        }
        Op::InsertBefore { line, body } => {
            require(*line)?;
            let at = line_index(*line, lines.len())?;
            (at, at, body.clone())
        }
        Op::InsertAfter { line, body } => {
            require(*line)?;
            let at = line_index(*line, lines.len())? + 1;
            (at, at, body.clone())
        }
        Op::InsertAfterBlock { line, body } => {
            let (_, at) = block_span(lines, *line, path, Some(seen))?;
            (at, at, body.clone())
        }
        Op::Append { body } => {
            if let Some(last) = lines.len().checked_sub(1) {
                require((last + 1) as u32)?;
            }
            (lines.len(), lines.len(), body.clone())
        }
        Op::InsertBof { body } => {
            if !lines.is_empty() {
                require(1)?;
            }
            (0, 0, body.clone())
        }
        Op::Cut { start, end, .. } => {
            require_range(seen, *start, *end)?;
            let (lo, hi) = inclusive_range(*start, *end, lines.len())?;
            (lo, hi, Vec::new())
        }
        Op::CutBlock { line, .. } => {
            let (lo, hi) = block_span(lines, *line, path, Some(seen))?;
            (lo, hi, Vec::new())
        }
        Op::PasteBefore { line, register } => {
            require(*line)?;
            let at = line_index(*line, lines.len())?;
            (at, at, registers.read(register.as_deref())?)
        }
        Op::PasteAfter { line, register } => {
            require(*line)?;
            let at = line_index(*line, lines.len())? + 1;
            (at, at, registers.read(register.as_deref())?)
        }
        Op::PasteEof { register } => {
            if let Some(last) = lines.len().checked_sub(1) {
                require((last + 1) as u32)?;
            }
            (
                lines.len(),
                lines.len(),
                registers.read(register.as_deref())?,
            )
        }
        Op::PasteBof { register } => {
            if !lines.is_empty() {
                require(1)?;
            }
            (0, 0, registers.read(register.as_deref())?)
        }
        Op::PasteRange {
            start,
            end,
            register,
        } => {
            require_range(seen, *start, *end)?;
            let (lo, hi) = inclusive_range(*start, *end, lines.len())?;
            (lo, hi, registers.read(Some(register))?)
        }
        Op::PasteBlock { line, register } => {
            let (lo, hi) = block_span(lines, *line, path, Some(seen))?;
            (lo, hi, registers.read(Some(register))?)
        }
        Op::PasteAfterBlock { line, register } => {
            let (_, at) = block_span(lines, *line, path, Some(seen))?;
            (at, at, registers.read(register.as_deref())?)
        }
    };
    Ok(Span {
        start,
        end,
        replacement,
        op_index,
    })
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

/// 一处替换在新旧文本中的字节区间对。
pub(super) type Change = (std::ops::Range<usize>, std::ops::Range<usize>);

/// `N*` 块操作的公共三步：解析块 → 行号已展示校验 → 转 0 索引区间。
fn block_span(
    lines: &[&str],
    line: u32,
    path: &str,
    seen: Option<&HashSet<u32>>,
) -> Result<(usize, usize), String> {
    let (start, end) = block::resolve_block(lines, line, path)?;
    if let Some(seen) = seen {
        require_range(seen, start, end)?;
    }
    inclusive_range(start, end, lines.len())
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
    let Prepared {
        label,
        canonical,
        original,
        updated,
        changes,
        file_op,
        base_seen,
    } = prepared;
    match file_op {
        Some(FileOp::Rem) => {
            let lock = edit_lock(&canonical)?;
            let _guard = lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            fs::remove_file(&canonical)
                .map_err(|e| format!("Failed to delete {}: {e}", canonical.display()))?;
            Ok(format!("Deleted file {}", label))
        }
        Some(FileOp::Move { dest }) => {
            let dest_path = resolve_path(working_dir, &dest);
            if dest_path.exists() {
                return Err(format!(
                    "Failed to move {} -> {}: destination exists",
                    label, dest
                ));
            }
            let src_lock = edit_lock(&canonical)?;
            let dest_lock = edit_lock(&dest_path)?;
            let _g1 = src_lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            let _g2 = dest_lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            write_new_file(&dest_path, &updated)?;
            fs::remove_file(&canonical)
                .map_err(|e| format!("Failed to remove {}: {e}", canonical.display()))?;
            let dest_canonical = fs::canonicalize(&dest_path).unwrap_or(dest_path.clone());
            report_update(
                session,
                dest,
                dest_canonical,
                &original,
                &updated,
                changes,
                &base_seen,
            )
        }
        None => {
            if updated == original {
                return Err("unchanged: edits produce no file changes".into());
            }
            let lock = edit_lock(&canonical)?;
            let _guard = lock
                .lock()
                .map_err(|_| "File mutation lock poisoned".to_string())?;
            persist_edit(&canonical, &original, &updated)?;
            report_update(
                session, label, canonical, &original, &updated, changes, &base_seen,
            )
        }
    }
}
