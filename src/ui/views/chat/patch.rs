//! freeform 编辑工具（apply_patch / hashline_edit）：补丁原文的容错解析、头部摘要与展开块渲染。

use super::*;
use super::{diff::*, tool_block::*};
use crate::ui::views::{bold, fg};

/// 补丁中的单个文件操作
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FileKind {
    Update,
    Add,
    Delete,
    Move(String),
}

/// 展开块中的一行补丁内容
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Row {
    /// 弱化说明行：apply_patch 的 `@@ ctx` 或 hashline 的 `PUT 4.=6`
    Note(String),
    Context(String),
    Insert(String),
    Delete(String),
}

#[derive(Debug)]
pub(super) struct FileChange {
    pub(super) path: String,
    pub(super) kind: FileKind,
    pub(super) rows: Vec<Row>,
    added: usize,
    removed: usize,
    /// 计数是否精确（hashline 的块/寄存器操作无法从输入得知行数）
    exact: bool,
}

impl FileChange {
    fn new(path: &str, kind: FileKind) -> Self {
        Self {
            path: path.to_string(),
            kind,
            rows: Vec::new(),
            added: 0,
            removed: 0,
            exact: true,
        }
    }

    fn tail(&self) -> Tail {
        match &self.kind {
            FileKind::Delete => Tail::Deleted,
            FileKind::Add => Tail::New(self.added),
            FileKind::Update | FileKind::Move(_) if !self.exact => Tail::None,
            FileKind::Move(_) if self.added + self.removed == 0 => Tail::None,
            _ => Tail::Counts(self.added, self.removed),
        }
    }

    fn target(&self) -> String {
        match &self.kind {
            FileKind::Move(dest) => format!("{} → {dest}", self.path),
            _ => self.path.clone(),
        }
    }
}

/// 头部摘要的计数尾注
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Tail {
    Counts(usize, usize),
    New(usize),
    Deleted,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Summary {
    pub(super) target: String,
    pub(super) tail: Tail,
}

impl Summary {
    pub(super) fn text(&self) -> String {
        match self.tail {
            Tail::Counts(added, removed) => format!("{} (+{added}, -{removed})", self.target),
            Tail::New(added) => format!("{} (new, +{added})", self.target),
            Tail::Deleted => format!("{} (deleted)", self.target),
            Tail::None => self.target.clone(),
        }
    }

    /// 头部着色：新增绿、删除红，其余沿用弱化样式
    pub(super) fn spans(&self, detail_style: Style) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(self.target.clone(), detail_style)];
        let plain = |text: &str| Span::styled(text.to_string(), detail_style);
        match self.tail {
            Tail::Counts(added, removed) => spans.extend([
                plain(" ("),
                Span::styled(format!("+{added}"), EDIT_INSERT),
                plain(", "),
                Span::styled(format!("-{removed}"), EDIT_DELETE),
                plain(")"),
            ]),
            Tail::New(added) => spans.extend([
                plain(" (new, "),
                Span::styled(format!("+{added}"), EDIT_INSERT),
                plain(")"),
            ]),
            Tail::Deleted => spans.push(plain(" (deleted)")),
            Tail::None => {}
        }
        spans
    }
}

pub(super) fn summarize(changes: &[FileChange]) -> Option<Summary> {
    match changes {
        [] => None,
        [only] => Some(Summary {
            target: only.target(),
            tail: only.tail(),
        }),
        many => {
            let target = format!("{} files", many.len());
            let tail = many
                .iter()
                .try_fold((0, 0), |(added, removed), change| match change.tail() {
                    Tail::Counts(a, r) => Some((added + a, removed + r)),
                    Tail::New(a) => Some((added + a, removed)),
                    Tail::Deleted => Some((added, removed)),
                    Tail::None => None,
                })
                .map_or(Tail::None, |(added, removed)| Tail::Counts(added, removed));
            Some(Summary { target, tail })
        }
    }
}

/// 补丁文本的完整行：尚未收到换行的末行（流式截断）与无结果的调用一并丢弃
fn complete_lines(text: &str, complete: bool) -> impl Iterator<Item = &str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    // 末段：以换行结尾时为空段；否则是尚未结束的行，仅在调用已有结果时视为完整
    if text.ends_with('\n') || !complete {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// 解析 V4A 补丁，容忍截断：无法识别的行忽略
pub(super) fn parse_apply_patch(text: &str, complete: bool) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();
    for line in complete_lines(text, complete) {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            changes.push(FileChange::new(path.trim(), FileKind::Update));
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            changes.push(FileChange::new(path.trim(), FileKind::Add));
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            changes.push(FileChange::new(path.trim(), FileKind::Delete));
        } else if let Some(dest) = line.strip_prefix("*** Move to: ") {
            if let Some(change) = changes.last_mut() {
                change.kind = FileKind::Move(dest.trim().to_string());
            }
        } else if line.starts_with("***") {
            continue;
        } else if let Some(change) = changes.last_mut() {
            if line.starts_with("@@") {
                change.rows.push(Row::Note(line.trim_end().to_string()));
            } else if let Some(text) = line.strip_prefix('+') {
                change.added += 1;
                change.rows.push(Row::Insert(text.to_string()));
            } else if let Some(text) = line.strip_prefix('-') {
                change.removed += 1;
                change.rows.push(Row::Delete(text.to_string()));
            } else if let Some(text) = line.strip_prefix(' ') {
                change.rows.push(Row::Context(text.to_string()));
            } else if line.is_empty() {
                change.rows.push(Row::Context(String::new()));
            }
        }
    }
    changes
}

/// 定位符覆盖的原文行数；块/寄存器等需读文件才能确定的返回 None
fn locator_removed(token: &str) -> Option<usize> {
    if token.starts_with('<') || token.starts_with('>') {
        return Some(0);
    }
    if let Some((start, end)) = token.split_once(".=") {
        let (start, end) = (start.parse::<usize>().ok()?, end.parse::<usize>().ok()?);
        return end.checked_sub(start).map(|span| span + 1);
    }
    token.parse::<usize>().ok().map(|_| 1)
}

fn unquote(path: &str) -> &str {
    path.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(path)
}

/// 解析 hashline 补丁，容忍截断：无法识别的行忽略
pub(super) fn parse_hashline_input(text: &str, complete: bool) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();
    let mut in_body = false;
    for line in complete_lines(text, complete) {
        let trimmed = line.trim_end();
        if let Some(inner) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_body = false;
            let change = match inner.rsplit_once('#') {
                Some((path, _)) => FileChange::new(path, FileKind::Update),
                None => FileChange::new(inner, FileKind::Add),
            };
            changes.push(change);
            continue;
        }
        let Some(change) = changes.last_mut() else {
            continue;
        };
        if in_body && let Some(body) = line.strip_prefix('+') {
            change.added += 1;
            change.rows.push(Row::Insert(body.to_string()));
            continue;
        }
        let mut words = trimmed.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        let keyword = keyword.trim_end_matches(':');
        let argument = words.next().unwrap_or_default();
        match keyword {
            "ADD" => in_body = true,
            "REM" => {
                in_body = false;
                change.kind = FileKind::Delete;
            }
            "MV" => {
                in_body = false;
                change.kind = FileKind::Move(unquote(argument).to_string());
            }
            "PUT" | "CUT" => {
                let has_body = keyword == "PUT" && trimmed.ends_with(':');
                in_body = has_body;
                let token = argument.trim_end_matches(':');
                match locator_removed(token) {
                    Some(removed) if keyword == "CUT" || has_body => change.removed += removed,
                    _ => change.exact = false,
                }
                if keyword == "PUT" && !has_body {
                    // 寄存器粘贴的行数取决于运行期寄存器内容
                    change.exact = false;
                }
                let note = trimmed.trim().trim_end_matches(':').trim_end();
                change.rows.push(Row::Note(note.to_string()));
            }
            _ => in_body = false,
        }
    }
    changes
}

const BADGE_FG: Color = Color::Rgb(24, 24, 28);

fn badge(kind: &FileKind) -> (&'static str, Color) {
    match kind {
        FileKind::Update => ("Update", Color::Rgb(110, 160, 220)),
        FileKind::Add => ("Add", Color::Rgb(100, 180, 110)),
        FileKind::Delete => ("Delete", Color::Rgb(220, 100, 100)),
        FileKind::Move(_) => ("Move", Color::Rgb(210, 170, 90)),
    }
}

fn push_title(lines: &mut Vec<Line<'static>>, kind: &FileKind, target: &str, width: usize) {
    let (label, color) = badge(kind);
    let spans = vec![
        Span::styled(
            format!(" {label} "),
            bold(BADGE_FG).bg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(target.to_string(), fg(Color::Rgb(210, 210, 210))),
    ];
    lines.extend(wrap_spans_to_lines(&spans, width, "", "  "));
}

fn push_note(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    push_edit_numbered_row(
        lines,
        EditCell::plain("  ", text, fg(Color::DarkGray)),
        width,
    );
}

fn push_inserted(lines: &mut Vec<Line<'static>>, text: &str, line: Option<usize>, width: usize) {
    let cell = match line {
        Some(number) => EditCell::new(number, "+ ", text, EDIT_INSERT),
        None => EditCell::plain("+ ", text, EDIT_INSERT),
    };
    push_edit_numbered_row(lines, cell.with_background(EDIT_INSERT_BG), width);
}

/// 按输入渲染各文件的变更
pub(super) fn push_changes(lines: &mut Vec<Line<'static>>, changes: &[FileChange], width: usize) {
    for (index, change) in changes.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        push_title(lines, &change.kind, &change.target(), width);
        match change.kind {
            FileKind::Delete => push_note(lines, "file deleted", width),
            FileKind::Add => {
                for (number, row) in change.rows.iter().enumerate() {
                    if let Row::Insert(text) = row {
                        push_inserted(lines, text, Some(number + 1), width);
                    }
                }
            }
            FileKind::Update | FileKind::Move(_) => {
                for row in &change.rows {
                    match row {
                        Row::Note(text) => push_note(lines, text, width),
                        Row::Context(text) => push_edit_numbered_row(
                            lines,
                            EditCell::plain("  ", text, Style::default()),
                            width,
                        ),
                        Row::Insert(text) => push_inserted(lines, text, None, width),
                        Row::Delete(text) => push_edit_numbered_row(
                            lines,
                            EditCell::plain("- ", text, EDIT_DELETE)
                                .with_background(EDIT_DELETE_BG),
                            width,
                        ),
                    }
                }
            }
        }
    }
}

/// hashline 结果中的一份文件报告
#[derive(Debug, PartialEq, Eq)]
enum Report<'a> {
    Edited { label: &'a str, diff: Vec<&'a str> },
    Deleted { label: &'a str },
    Created { label: &'a str },
}

/// 解析 hashline_edit 的结果文本：`[label#TAG]` + unified diff、`Deleted file X`、`Created file X`
fn parse_hashline_result(content: &str) -> Vec<Report<'_>> {
    let is_header = |line: &str| {
        line.strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|inner| inner.rsplit_once('#'))
            .is_some_and(|(label, tag)| {
                !label.is_empty() && tag.len() == 4 && tag.bytes().all(|b| b.is_ascii_hexdigit())
            })
    };
    let mut reports = Vec::new();
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(label) = line.strip_prefix("Deleted file ") {
            reports.push(Report::Deleted { label });
        } else if let Some(label) = line.strip_prefix("Created file ") {
            reports.push(Report::Created { label });
            // 紧随其后的 `[label#TAG]` 是新文件的快照头
            if lines.peek().is_some_and(|next| is_header(next)) {
                lines.next();
            }
        } else if is_header(line) {
            let label = &line[1..line.rfind('#').unwrap_or(1)];
            let mut diff = Vec::new();
            // 空行与 `Current snapshot preview` 之前是 diff 正文
            while let Some(next) = lines.peek() {
                if next.is_empty() || is_header(next) {
                    break;
                }
                diff.push(*next);
                lines.next();
            }
            reports.push(Report::Edited { label, diff });
        }
    }
    reports
}

/// `@@ -a[,b] +c[,d] @@` → (旧起始行, 新起始行)；计数为 0 时起始值是前一行偏移
fn hunk_start(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    parts.next()?;
    let start = |part: Option<&str>, sign: char| -> Option<usize> {
        let range = part?.strip_prefix(sign)?;
        let (start, count) = range.split_once(',').unwrap_or((range, "1"));
        let start: usize = start.parse().ok()?;
        Some(if count == "0" { start + 1 } else { start })
    };
    Some((start(parts.next(), '-')?, start(parts.next(), '+')?))
}

fn push_diff(lines: &mut Vec<Line<'static>>, diff: &[&str], width: usize) {
    let (mut old_line, mut new_line) = (1, 1);
    let mut seen_hunk = false;
    for (index, row) in diff.iter().enumerate() {
        if row.starts_with("@@") {
            if seen_hunk {
                push_note(lines, "⋯", width);
            }
            seen_hunk = true;
            if let Some((old, new)) = hunk_start(row) {
                (old_line, new_line) = (old, new);
            }
            continue;
        }
        // `--- path` / `+++ path` 仅出现在首个 hunk 之前
        if !seen_hunk && index < 2 && (row.starts_with("--- ") || row.starts_with("+++ ")) {
            continue;
        }
        if row.starts_with('\\') {
            continue;
        }
        if let Some(text) = row.strip_prefix('+') {
            let cell = EditCell::new(new_line, "+ ", text, EDIT_INSERT);
            push_edit_numbered_row(lines, cell.with_background(EDIT_INSERT_BG), width);
            new_line += 1;
        } else if let Some(text) = row.strip_prefix('-') {
            let cell = EditCell::new(old_line, "- ", text, EDIT_DELETE);
            push_edit_numbered_row(lines, cell.with_background(EDIT_DELETE_BG), width);
            old_line += 1;
        } else if let Some(text) = row.strip_prefix(' ') {
            let cell = EditCell::new(new_line, "  ", text, Style::default());
            push_edit_numbered_row(lines, cell, width);
            old_line += 1;
            new_line += 1;
        } else {
            push_note(lines, row, width);
        }
    }
}

/// 成功的 hashline_edit：渲染结果中的真实带行号 diff；ADD 的正文取自输入。
/// 结果无可识别的报告时返回 false，由调用方按输入渲染。
pub(super) fn push_hashline_result(
    lines: &mut Vec<Line<'static>>,
    content: &str,
    input: &[FileChange],
    width: usize,
) -> bool {
    let reports = parse_hashline_result(content);
    if reports.is_empty() {
        return false;
    }
    let mut added = input.iter().filter(|change| change.kind == FileKind::Add);
    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        match report {
            Report::Edited { label, diff } => {
                push_title(lines, &FileKind::Update, label, width);
                push_diff(lines, diff, width);
            }
            Report::Deleted { label } => {
                push_title(lines, &FileKind::Delete, label, width);
                push_note(lines, "file deleted", width);
            }
            Report::Created { label } => {
                push_title(lines, &FileKind::Add, label, width);
                if let Some(change) = added.next() {
                    for (number, row) in change.rows.iter().enumerate() {
                        if let Row::Insert(text) = row {
                            push_inserted(lines, text, Some(number + 1), width);
                        }
                    }
                }
            }
        }
    }
    true
}
