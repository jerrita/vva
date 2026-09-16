//! Edit 差异渲染：LCS 行级 diff 与统一/双栏两种排版。

use super::*;
use super::{tool::*, tool_block::*};

pub(super) enum EditRow<'a> {
    Context {
        old_line: usize,
        new_line: usize,
        text: &'a str,
    },
    Delete {
        old_line: usize,
        text: &'a str,
    },
    Insert {
        new_line: usize,
        text: &'a str,
    },
}

pub(super) fn edit_rows<'a>(
    old: &'a str,
    new: &'a str,
    old_start: usize,
    new_start: usize,
) -> Vec<EditRow<'a>> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut lcs = vec![vec![0; new_lines.len() + 1]; old_lines.len() + 1];
    for i in (0..old_lines.len()).rev() {
        for j in (0..new_lines.len()).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut rows = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut old_line, mut new_line) = (old_start, new_start);
    while i < old_lines.len() || j < new_lines.len() {
        if i < old_lines.len() && j < new_lines.len() && old_lines[i] == new_lines[j] {
            rows.push(EditRow::Context {
                old_line,
                new_line,
                text: old_lines[i],
            });
            i += 1;
            j += 1;
            old_line += 1;
            new_line += 1;
        } else if i < old_lines.len() && (j == new_lines.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            rows.push(EditRow::Delete {
                old_line,
                text: old_lines[i],
            });
            i += 1;
            old_line += 1;
        } else {
            rows.push(EditRow::Insert {
                new_line,
                text: new_lines[j],
            });
            j += 1;
            new_line += 1;
        }
    }
    rows
}

/// 单个 diff 行单元格：行号 + 标记 + 文本 + 样式
pub(super) struct EditCell<'a> {
    line_number: Option<usize>,
    marker: &'static str,
    text: &'a str,
    style: Style,
    background: Option<Color>,
}

impl<'a> EditCell<'a> {
    pub(super) fn new(
        line_number: usize,
        marker: &'static str,
        text: &'a str,
        style: Style,
    ) -> Self {
        Self {
            line_number: Some(line_number),
            marker,
            text,
            style,
            background: None,
        }
    }

    fn with_background(mut self, background: Color) -> Self {
        self.background = Some(background);
        self
    }
}

pub(super) fn edit_side_lines(
    cell: EditCell<'_>,
    width: usize,
    initial_indent: &str,
    cont_indent: &str,
) -> Vec<Line<'static>> {
    let background = cell.background;
    let mut spans = Vec::new();
    if let Some(line_number) = cell.line_number {
        spans.push(Span::styled(
            format!("{line_number:>4} "),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::styled(cell.marker, cell.style));
    spans.push(Span::raw(cell.text.to_string()));
    let mut lines = wrap_spans_to_lines(&spans, width, initial_indent, cont_indent);
    for line in &mut lines {
        if let Some(background) = cell.background {
            for span in &mut line.spans {
                span.style = span.style.bg(background);
            }
        }
        let current = line
            .spans
            .iter()
            .map(|span| span.content.width())
            .sum::<usize>();
        if current < width {
            line.spans.push(Span::styled(
                " ".repeat(width - current),
                background.map_or_else(Style::default, |color| Style::default().bg(color)),
            ));
        }
    }
    lines
}

pub(super) fn push_edit_unified(
    lines: &mut Vec<Line<'static>>,
    old: &str,
    new: &str,
    old_start: usize,
    new_start: usize,
    max_width: usize,
) {
    for row in edit_rows(old, new, old_start, new_start) {
        match row {
            EditRow::Context { old_line, text, .. } => push_edit_numbered_row(
                lines,
                EditCell::new(old_line, "  ", text, Style::default()),
                max_width,
            ),
            EditRow::Delete { old_line, text } => push_edit_numbered_row(
                lines,
                EditCell::new(old_line, "- ", text, EDIT_DELETE).with_background(EDIT_DELETE_BG),
                max_width,
            ),
            EditRow::Insert { new_line, text } => push_edit_numbered_row(
                lines,
                EditCell::new(new_line, "+ ", text, EDIT_INSERT).with_background(EDIT_INSERT_BG),
                max_width,
            ),
        }
    }
}

pub(super) fn push_edit_numbered_row(
    lines: &mut Vec<Line<'static>>,
    cell: EditCell<'_>,
    max_width: usize,
) {
    lines.extend(edit_side_lines(cell, max_width, "   ", "     "));
}

pub(super) fn push_edit_column_pair<'a>(
    lines: &mut Vec<Line<'static>>,
    left: Option<EditCell<'a>>,
    right: Option<EditCell<'a>>,
    left_w: usize,
    right_w: usize,
) {
    let blank = |width: usize| {
        edit_side_lines(
            EditCell {
                line_number: None,
                marker: "  ",
                text: "",
                style: Style::default(),
                background: None,
            },
            width,
            "",
            "",
        )
    };
    let left_lines = match left {
        Some(cell) => edit_side_lines(cell, left_w, "", ""),
        None => blank(left_w),
    };
    let right_lines = match right {
        Some(cell) => edit_side_lines(cell, right_w, "", ""),
        None => blank(right_w),
    };
    let row_count = left_lines.len().max(right_lines.len());
    for i in 0..row_count {
        let mut spans = vec![Span::raw(" ".repeat(EDIT_INDENT))];
        spans.extend(
            left_lines
                .get(i)
                .map(|line| line.spans.clone())
                .unwrap_or_else(|| vec![Span::raw(" ".repeat(left_w))]),
        );
        spans.push(Span::styled(" │ ", EDIT_SEP));
        spans.extend(
            right_lines
                .get(i)
                .map(|line| line.spans.clone())
                .unwrap_or_else(|| vec![Span::raw(" ".repeat(right_w))]),
        );
        lines.push(Line::from(spans));
    }
}

pub(super) fn push_edit_change_block<'a>(
    lines: &mut Vec<Line<'static>>,
    deleted: &mut Vec<(usize, &'a str)>,
    inserted: &mut Vec<(usize, &'a str)>,
    left_w: usize,
    right_w: usize,
) {
    let rows = deleted.len().max(inserted.len());
    for i in 0..rows {
        push_edit_column_pair(
            lines,
            deleted.get(i).map(|(line, text)| {
                EditCell::new(*line, "- ", text, EDIT_DELETE).with_background(EDIT_DELETE_BG)
            }),
            inserted.get(i).map(|(line, text)| {
                EditCell::new(*line, "+ ", text, EDIT_INSERT).with_background(EDIT_INSERT_BG)
            }),
            left_w,
            right_w,
        );
    }
    deleted.clear();
    inserted.clear();
}

pub(super) fn push_edit_columns<'a>(
    lines: &mut Vec<Line<'static>>,
    old: &'a str,
    new: &'a str,
    old_start: usize,
    new_start: usize,
    max_width: usize,
) {
    let available = max_width.saturating_sub(EDIT_INDENT + EDIT_SEP_WIDTH);
    let left_w = available / 2;
    let right_w = available - left_w;
    let mut deleted: Vec<(usize, &'a str)> = Vec::new();
    let mut inserted: Vec<(usize, &'a str)> = Vec::new();

    for row in edit_rows(old, new, old_start, new_start) {
        match row {
            EditRow::Context {
                old_line,
                new_line,
                text,
            } => {
                push_edit_change_block(lines, &mut deleted, &mut inserted, left_w, right_w);
                push_edit_column_pair(
                    lines,
                    Some(EditCell::new(old_line, "  ", text, Style::default())),
                    Some(EditCell::new(new_line, "  ", text, Style::default())),
                    left_w,
                    right_w,
                );
            }
            EditRow::Delete { old_line, text } => deleted.push((old_line, text)),
            EditRow::Insert { new_line, text } => inserted.push((new_line, text)),
        }
    }
    push_edit_change_block(lines, &mut deleted, &mut inserted, left_w, right_w);
}

pub(super) fn edit_hunk_starts(diff: Option<&str>) -> Option<(usize, usize)> {
    let header = diff?.lines().find(|line| line.starts_with("@@ -"))?;
    let mut ranges = header.split_whitespace();
    ranges.next()?;
    let old = ranges.next()?.strip_prefix('-')?;
    let new = ranges.next()?.strip_prefix('+')?;
    Some((
        old.split_once(',')
            .map_or(old, |(start, _)| start)
            .parse()
            .ok()?,
        new.split_once(',')
            .map_or(new, |(start, _)| start)
            .parse()
            .ok()?,
    ))
}

/// 按行首 +/- 着色输出补丁正文，超过 cap 行时以 "..." 提示截断
pub(super) fn push_patch_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    cap: usize,
    max_width: usize,
) {
    for line in text.lines().take(cap) {
        let style = match line.as_bytes().first() {
            Some(b'+') => EDIT_INSERT,
            Some(b'-') => EDIT_DELETE,
            _ => Style::default(),
        };
        push_filled_text(lines, line, style, max_width);
    }
    if text.lines().count() > cap {
        push_filled_text(
            lines,
            "...",
            Style::default().fg(Color::DarkGray),
            max_width,
        );
    }
}

pub(super) fn push_patch_detail(
    lines: &mut Vec<Line<'static>>,
    arguments: &str,
    result: Option<&str>,
    max_width: usize,
    cap: usize,
) {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments).ok();
    let fallback = parsed
        .as_ref()
        .and_then(|args| json_str(args, "input"))
        .unwrap_or(arguments);
    let text = result.unwrap_or(fallback);
    push_patch_lines(lines, text, cap, max_width);
}

pub(super) fn push_edit_detail(
    lines: &mut Vec<Line<'static>>,
    arguments: &str,
    result: Option<&str>,
    max_width: usize,
    cap: usize,
) {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return;
    };
    if args.get("edits").is_some() {
        if let Some(result) = result {
            push_patch_lines(lines, result, cap, max_width);
        } else if let Some(edits) = args.get("edits").and_then(serde_json::Value::as_array) {
            for edit in edits {
                for (field, marker, style) in [
                    ("old_string", "-", EDIT_DELETE),
                    ("new_string", "+", EDIT_INSERT),
                ] {
                    if let Some(text) = json_str(edit, field) {
                        for line in text.lines() {
                            push_filled_text(lines, &format!("{marker}{line}"), style, max_width);
                        }
                    }
                }
            }
        }
        return;
    }
    let Some(old) = json_str(&args, "old_string") else {
        return;
    };
    let Some(new) = json_str(&args, "new_string") else {
        return;
    };

    let (old_start, new_start) = edit_hunk_starts(result).unwrap_or((1, 1));
    let start = lines.len();
    if max_width >= EDIT_INDENT + EDIT_SEP_WIDTH + EDIT_MIN_COL * 2 {
        push_edit_columns(lines, old, new, old_start, new_start, max_width);
    } else {
        push_edit_unified(lines, old, new, old_start, new_start, max_width);
    }

    let produced = lines.len() - start;
    if produced > cap {
        let hidden = produced - cap;
        lines.truncate(start + cap);
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(
                format!("... ({hidden} more lines hidden)"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
}
