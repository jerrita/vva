//! 补丁应用：分组、预检、行号校验、寄存器与落盘提交。

use super::block;
use super::parse::{Anchor, FileOp, Op, Payload, Section, op_line};
use super::session::HashlineSession;
use super::snapshot::report_update;
use crate::tools::filesystem::{persist_edit, resolve_path, with_edit_locks, write_new_file};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) struct LineFile {
    pub(super) lines: Vec<String>,
    ending: &'static str,
    trailing_newline: bool,
    bom: bool,
}

impl LineFile {
    pub(super) fn parse(raw: &str) -> Self {
        let ending = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_newline = raw.ends_with('\n');
        let bom = raw.starts_with('\u{feff}');
        let raw = if bom {
            &raw['\u{feff}'.len_utf8()..]
        } else {
            raw
        };
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
            bom,
        }
    }

    pub(super) fn render(&self) -> String {
        let mut out = if self.bom {
            String::from("\u{feff}")
        } else {
            String::new()
        };
        out.push_str(&self.lines.join(self.ending));
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
    for prepared in &prepared {
        preflight_file_op(working_dir, prepared)
            .map_err(|error| format!("{error}; preflight failed, no files were changed"))?;
    }
    let mut reports = Vec::new();
    for (index, prepared) in prepared.into_iter().enumerate() {
        let report = commit_group(working_dir, session, prepared).map_err(|error| {
            if index == 0 {
                error
            } else {
                format!("{error}; {index} earlier section(s) were already applied")
            }
        })?;
        reports.push(report);
    }
    session.publish_named(batch_named)?;
    Ok(reports.join("\n\n"))
}

/// MV 目标解析：裸文件名相对源文件所在目录（原地重命名），带目录成分的
/// 相对路径相对工作区根，绝对路径原样。纯数字记法按行号误用拒绝。
fn move_dest(working_dir: &Path, canonical: &Path, dest: &str) -> Result<PathBuf, String> {
    let mut trimmed = dest.trim();
    if trimmed.len() > 2 && trimmed.starts_with('<') && trimmed.ends_with('>') {
        trimmed = &trimmed[1..trimmed.len() - 1];
    }
    if trimmed.starts_with('<') || trimmed.starts_with('>') {
        return Err(format!(
            "invalid_arguments: MV destination must not start with '<' or '>'; use `MV {}`",
            trimmed.trim_start_matches(['<', '>'])
        ));
    }
    if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit() || b == b' ') {
        return Err(format!(
            "invalid_arguments: `{dest}` looks like line numbers; MV takes a destination path"
        ));
    }
    let has_dir = Path::new(trimmed)
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty());
    Ok(if has_dir {
        resolve_path(working_dir, trimmed)
    } else {
        canonical.parent().unwrap_or(working_dir).join(trimmed)
    })
}

/// 相对工作区根的展示路径；工作区之外退回绝对路径。
fn display_path(working_dir: &Path, path: &Path) -> String {
    path.strip_prefix(working_dir).map_or_else(
        |_| path.display().to_string(),
        |rel| rel.display().to_string(),
    )
}

/// 全部 prepare 完成后、任何落盘前的文件级操作校验：MV 目标目录必须存在、
/// 目标必须不存在，避免部分 section 已提交才暴露目录缺失。
fn preflight_file_op(working_dir: &Path, prepared: &Prepared) -> Result<(), String> {
    let Some(FileOp::Move { dest }) = &prepared.file_op else {
        return Ok(());
    };
    let dest_path = move_dest(working_dir, &prepared.canonical, dest)?;
    if let Some(parent) = dest_path.parent()
        && !parent.is_dir()
    {
        return Err(format!(
            "Failed to move {} -> {}: directory {} does not exist; create it first",
            prepared.label,
            display_path(working_dir, &dest_path),
            parent.display()
        ));
    }
    if dest_path.exists() {
        return Err(format!(
            "Failed to move {} -> {}: destination {} exists",
            prepared.label,
            dest,
            display_path(working_dir, &dest_path)
        ));
    }
    Ok(())
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
                    "invalid_arguments: {} has mixed snapshot tags {} and {}; re-Read and copy one [{}#TAG] header",
                    section.path, existing.tag, section.tag, section.path
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
            return Err("Unsupported binary file; use Write".into());
        }
        raw
    } else {
        return Err(format!(
            "Failed to resolve {}; check the path relative to the working directory",
            group.canonical.display()
        ));
    };
    let snap = session.lookup(&group.canonical, &group.tag)?;
    if snap.text != original {
        // 报错中给出的新 tag 先注册（seen 为空），照抄可过校验，
        // 后续按 unseen 提示 Read，而不是 not registered 空转一轮。
        let fresh_tag = session.record(group.canonical.clone(), &original, std::iter::empty())?;
        return Err(format!(
            "stale: [{}#{}] no longer matches disk (now [{}#{fresh_tag}]); Read and retry",
            group.label, group.tag, group.label,
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
        let (first, second) = (&pair[0], &pair[1]);
        if first.end > second.start {
            if second.start == second.end && second.end < first.end {
                return Err(format!(
                    "overlap: op {} ({}) anchors inside the lines removed by op {} \
                     ({} covers {}..={}); paste after the span (>{})",
                    second.op_index + 1,
                    op_line(&group.ops[second.op_index]),
                    first.op_index + 1,
                    op_line(&group.ops[first.op_index]),
                    first.start + 1,
                    first.end,
                    first.end,
                ));
            }
            return Err(format!(
                "overlap: op {} ({}) and op {} ({}) touch overlapping original lines; merge into one PUT",
                first.op_index + 1,
                op_line(&group.ops[first.op_index]),
                second.op_index + 1,
                op_line(&group.ops[second.op_index]),
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
    let originally_empty = file.lines.is_empty();
    let mut updated = file;
    updated.lines = updated_lines;
    if updated.lines.is_empty() {
        updated.trailing_newline = false;
    } else if originally_empty {
        // 空文件无换行约定可保真：对齐 Write 建档，补末尾换行。
        updated.trailing_newline = true;
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
        Op {
            anchor: Anchor::Range { start, end },
            payload: Payload::Cut(register),
        } => (*start, *end, register.clone()),
        Op {
            anchor: Anchor::Block { line },
            payload: Payload::Cut(register),
        } => {
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
                .ok_or_else(|| {
                    format!("not_found: register @{name} is empty; CUT into it first with `CUT N.=M @{name}`")
                }),
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
    let (start, end) = match op.anchor {
        Anchor::Range { start, end } => {
            let (lo, hi) = inclusive_range(start, end, lines.len())?;
            require_range(seen, start, end)?;
            (lo, hi)
        }
        Anchor::Block { line } => block_span(lines, line, path, seen)?,
        Anchor::Before { line } => {
            let at = line_index(line, lines.len())?;
            require(line)?;
            (at, at)
        }
        Anchor::After { line } => {
            let at = line_index(line, lines.len())? + 1;
            require(line)?;
            (at, at)
        }
        Anchor::AfterBlock { line } => {
            let (_, at) = block_span(lines, line, path, seen)?;
            (at, at)
        }
        Anchor::Eof => {
            if let Some(last) = lines.len().checked_sub(1) {
                require((last + 1) as u32)?;
            }
            (lines.len(), lines.len())
        }
        Anchor::Bof => {
            if !lines.is_empty() {
                require(1)?;
            }
            (0, 0)
        }
    };
    let replacement = match &op.payload {
        Payload::Body(body) => body.clone(),
        Payload::Register(register) => registers.read(register.as_deref())?,
        Payload::Cut(_) => Vec::new(),
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
            "unseen: missing displayed lines {}; Read these ranges first",
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
    seen: &HashSet<u32>,
) -> Result<(usize, usize), String> {
    let (start, end) = block::resolve_block(lines, line, path)?;
    require_range(seen, start, end)?;
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
        Some(FileOp::Rem) => with_edit_locks(&[&canonical], || {
            fs::remove_file(&canonical)
                .map_err(|e| format!("Failed to delete {}: {e}", canonical.display()))?;
            Ok(format!("Deleted file {label}"))
        }),
        Some(FileOp::Move { dest }) => {
            let dest_path = move_dest(working_dir, &canonical, &dest)?;
            with_edit_locks(&[&canonical, &dest_path], || {
                write_new_file(&dest_path, &updated)?;
                fs::remove_file(&canonical)
                    .map_err(|e| format!("Failed to remove {}: {e}", canonical.display()))
            })?;
            let dest_canonical = fs::canonicalize(&dest_path).unwrap_or(dest_path.clone());
            let dest_label = display_path(working_dir, &dest_path);
            report_update(
                session,
                dest_label,
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
            with_edit_locks(&[&canonical], || {
                persist_edit(&canonical, &original, &updated)
            })?;
            report_update(
                session, label, canonical, &original, &updated, changes, &base_seen,
            )
        }
    }
}
