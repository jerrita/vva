//! 文本选区：可选中列判定、文本提取与高亮。

use super::user_block::*;
use super::*;
use crate::ui::spans::split_spans_by_hit;

pub(super) fn selectable_columns(line: &Line) -> (usize, usize) {
    let is_user_block = line
        .spans
        .first()
        .is_some_and(|span| span.content == "▌" && span.style.fg == Some(USER_BLOCK_BAR));
    if !is_user_block {
        return (0, usize::MAX);
    }
    let mut col = 0;
    let mut bounds = (0, 0);
    for (index, span) in line.spans.iter().enumerate() {
        let end = col + span.width();
        if index >= 2 && span.style.fg.is_some() {
            if bounds == (0, 0) {
                bounds.0 = col;
            }
            bounds.1 = end;
        }
        col = end;
    }
    bounds
}

/// 从选区提取正文，保留视觉换行并去除尾随空白。
pub fn extract_selected_text(lines: &[Line], selection: &TextSelection) -> Option<String> {
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.normalized();
    if start.line >= lines.len() {
        return None;
    }

    let mut result_lines = Vec::new();

    let end_idx = end.line.min(lines.len().saturating_sub(1));
    for (line_idx, line) in lines.iter().enumerate().take(end_idx + 1).skip(start.line) {
        let bounds = selectable_columns(line);
        if bounds == (0, 0) {
            continue;
        }
        let Some(range) = selection.line_column_range(line_idx) else {
            result_lines.push(String::new());
            continue;
        };

        let mut line_text = String::new();
        let mut col = 0;

        for span in &line.spans {
            for ch in span.content.chars() {
                if char_in_column_range(ch, col, range) && char_in_column_range(ch, col, bounds) {
                    line_text.push(ch);
                }
                col += ch.width().unwrap_or(0);
            }
        }

        result_lines.push(line_text.trim_end().to_string());
    }

    if result_lines.is_empty() {
        return None;
    }

    while let Some(last) = result_lines.last() {
        if last.is_empty() && result_lines.len() > 1 {
            result_lines.pop();
        } else {
            break;
        }
    }

    Some(result_lines.join("\n"))
}

/// 对处于选区范围内的文本行注入高亮样式（支持 CJK 字符边界精确切分）
pub fn apply_selection_highlight(lines: &mut [Line<'static>], selection: &TextSelection) {
    apply_selection_highlight_offset(lines, selection, 0);
}

/// 同上，`offset` 为切片首行在整体行列表中的行号（渲染可视窗口时使用）
pub(crate) fn apply_selection_highlight_offset(
    lines: &mut [Line<'static>],
    selection: &TextSelection,
    offset: usize,
) {
    if selection.is_empty() {
        return;
    }
    let (start, end) = selection.normalized();
    if end.line < offset {
        return;
    }
    // 起点落在窗口上方时，窗口首行按整行选中处理
    let start = if start.line < offset {
        TextCoord { line: 0, col: 0 }
    } else {
        TextCoord {
            line: start.line - offset,
            col: start.col,
        }
    };
    let end = TextCoord {
        line: end.line - offset,
        col: end.col,
    };
    let selection = &TextSelection::new(start, end);
    let max_line = end.line.min(lines.len().saturating_sub(1));
    if start.line > max_line {
        return;
    }

    for (line_idx, line) in lines
        .iter_mut()
        .enumerate()
        .take(max_line + 1)
        .skip(start.line)
    {
        let Some(range) = selection.line_column_range(line_idx) else {
            continue;
        };

        let bounds = selectable_columns(line);
        let old_spans = std::mem::take(&mut line.spans);
        let mut new_spans: Vec<Span<'static>> = Vec::new();
        let mut col = 0;

        for span in old_spans {
            new_spans.extend(split_spans_by_hit(
                &span.content,
                span.style,
                SELECTION_STYLE,
                |_, ch| {
                    let hit = char_in_column_range(ch, col, range)
                        && char_in_column_range(ch, col, bounds);
                    col += ch.width().unwrap_or(0);
                    hit
                },
            ));
        }

        line.spans = new_spans;
    }
}

/// 在已绘制的可视行上覆盖选区底色，避免克隆行并重建 span
pub(super) fn overlay_selection(
    buf: &mut Buffer,
    area: Rect,
    lines: &[Line],
    selection: &TextSelection,
    scroll: usize,
) {
    if selection.is_empty() {
        return;
    }
    let text_width = area.width.saturating_sub(1) as usize;
    if text_width == 0 {
        return;
    }
    for (i, line) in lines.iter().enumerate() {
        let Some((start_col, end_col)) = selection.line_column_range(scroll + i) else {
            continue;
        };
        let bounds = selectable_columns(line);
        let start_col = start_col.max(bounds.0).min(text_width);
        let end_col = end_col.min(bounds.1).min(text_width);
        if start_col >= end_col {
            continue;
        }
        buf.set_style(
            Rect {
                x: area.x + start_col as u16,
                y: area.y + i as u16,
                width: (end_col - start_col) as u16,
                height: 1,
            },
            SELECTION_STYLE,
        );
    }
}
