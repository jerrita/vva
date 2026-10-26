//! 工具调用展开块：边框、填充与结果/差异正文。

use super::*;
use super::{diff::*, patch::*, tool::*};
use crate::tools::names;
use crate::ui::views::fg;

pub(super) const EDIT_DELETE: Style = Style::new().fg(Color::Rgb(220, 100, 100));
pub(super) const EDIT_INSERT: Style = Style::new().fg(Color::Rgb(100, 180, 110));
pub(super) const EDIT_DELETE_BG: Color = Color::Rgb(58, 37, 45);
pub(super) const EDIT_INSERT_BG: Color = Color::Rgb(30, 50, 63);
pub(super) const EDIT_SEP: Style = Style::new().fg(Color::DarkGray);
pub(super) const EDIT_INDENT: usize = 3;
pub(super) const EDIT_SEP_WIDTH: usize = 3;
pub(super) const EDIT_MIN_COL: usize = 20;
pub(super) const TOOL_BLOCK_BG: Color = Color::Rgb(40, 42, 46);
pub(super) const TOOL_BLOCK_BORDER: Style =
    Style::new().fg(Color::Rgb(100, 100, 110)).bg(TOOL_BLOCK_BG);
pub(super) const TOOL_PREVIEW_LIMIT: usize = 10;
pub(super) const TOOL_BLOCK_CHROME: usize = 4;

pub(super) fn tool_block_frame(left: &str, right: &str, max_width: usize) -> Line<'static> {
    let inner = max_width.saturating_sub(TOOL_BLOCK_CHROME);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(left.to_string(), TOOL_BLOCK_BORDER),
        Span::styled("─".repeat(inner), TOOL_BLOCK_BORDER),
        Span::styled(right.to_string(), TOOL_BLOCK_BORDER),
        Span::raw(" "),
    ])
}

pub(super) fn fill_to_width(mut spans: Vec<Span<'static>>, max_width: usize) -> Line<'static> {
    let bg = Style::default().bg(TOOL_BLOCK_BG);
    let inner = max_width.saturating_sub(TOOL_BLOCK_CHROME);
    let mut width = 0;
    for span in &mut spans {
        width += span.content.width();
        if span.style.bg.is_none() {
            span.style = span.style.patch(bg);
        }
    }
    if inner > width {
        spans.push(Span::styled(" ".repeat(inner - width), bg));
    }
    let mut out = Vec::with_capacity(spans.len() + 4);
    out.push(Span::raw(" "));
    out.push(Span::styled("│", TOOL_BLOCK_BORDER));
    out.extend(spans);
    out.push(Span::styled("│", TOOL_BLOCK_BORDER));
    out.push(Span::raw(" "));
    Line::from(out)
}

pub(super) fn push_filled_blank(lines: &mut Vec<Line<'static>>, max_width: usize) {
    lines.push(fill_to_width(Vec::new(), max_width));
}

pub(super) fn push_filled_text(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    style: Style,
    max_width: usize,
) {
    let inner = max_width.saturating_sub(TOOL_BLOCK_CHROME).max(1);
    if text.is_empty() {
        push_filled_blank(lines, max_width);
        return;
    }
    // ratatui 渲染时过滤控制字符，span 内的 \n 会静默丢失并拼接相邻行，须先按行拆分
    for text in text.lines() {
        let spans = vec![Span::styled(text.to_string(), style)];
        for wrapped in wrap_spans_to_lines(&spans, inner, " ", "  ") {
            lines.push(fill_to_width(wrapped.spans, max_width));
        }
    }
}

pub(super) fn tool_result_rows(result: &ToolResult) -> Vec<(String, Style)> {
    let style = if result.is_error {
        fg(Color::Rgb(220, 120, 120))
    } else {
        fg(Color::Rgb(180, 180, 180))
    };
    result
        .content
        .lines()
        .map(|line| (line.to_string(), style))
        .collect()
}

pub(super) fn push_tool_block(
    lines: &mut Vec<Line<'static>>,
    call: &ToolCall,
    result: Option<&ToolResult>,
    shell_output: Option<&crate::model::ShellOutput>,
    query_errors: &[&str],
    full: bool,
    max_width: usize,
) -> bool {
    let cmd_style = fg(Color::Rgb(210, 210, 210));
    let hint_style = fg(Color::Rgb(110, 110, 110));
    lines.push(tool_block_frame("╭", "╮", max_width));
    push_filled_blank(lines, max_width);
    let failed = result.is_some_and(|result| result.is_error);
    // 变更类工具头部已含路径；freeform 工具失败时回显补丁原文以便对照错误
    if !is_mutation_tool(&call.name) || (is_freeform_edit(&call.name) && failed) {
        push_filled_text(
            lines,
            &tool_block_command(&call.name, &call.arguments),
            cmd_style,
            max_width,
        );
        push_filled_blank(lines, max_width);
    }

    if let Some(output) = shell_output {
        // 预览只排版尾部有限行；全文档位仍受采集缓冲上限约束。
        let cap = if full { usize::MAX } else { TOOL_PREVIEW_LIMIT };
        let mut body = Vec::new();
        let text = if full {
            output.text.as_str()
        } else {
            let mut start = output
                .text
                .len()
                .saturating_sub(max_width.saturating_mul(cap).saturating_mul(4));
            while !output.text.is_char_boundary(start) {
                start += 1;
            }
            &output.text[start..]
        };
        let tail: Vec<_> = text.lines().rev().take(cap).collect();
        for line in tail.iter().rev() {
            push_filled_text(&mut body, line, cmd_style, max_width);
        }
        let from = body.len().saturating_sub(cap);
        let truncated = from > 0 || text.len() < output.text.len() || text.lines().count() > cap;
        lines.extend(body.into_iter().skip(from));
        if output.start > 0 {
            push_filled_text(lines, "输出已截断，展示末尾内容", hint_style, max_width);
        }
        if let Some(error) = &output.error {
            push_filled_text(lines, error, fg(Color::Rgb(220, 120, 120)), max_width);
        }
        for error in query_errors {
            push_filled_text(
                lines,
                &format!("查询失败: {error}"),
                fg(Color::Rgb(220, 120, 120)),
                max_width,
            );
        }
        push_block_tail(lines, truncated, max_width, hint_style);
        return truncated;
    }

    let cap = if full { usize::MAX } else { TOOL_PREVIEW_LIMIT };
    if is_mutation_tool(&call.name) && !failed {
        let mut body = Vec::new();
        let inner_width = max_width.saturating_sub(TOOL_BLOCK_CHROME);
        if call.name == names::WRITE {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments)
                && let Some(content) = json_str(&args, "content")
            {
                for (index, text) in content.lines().enumerate() {
                    push_edit_numbered_row(
                        &mut body,
                        EditCell::new(index + 1, "  ", text, Style::default()),
                        inner_width,
                    );
                }
            }
        } else if call.name == names::APPLY_PATCH {
            let changes = parse_apply_patch(&call.arguments, result.is_some());
            push_changes(&mut body, &changes, inner_width);
        } else if call.name == names::HASHLINE_EDIT {
            let changes = parse_hashline_input(&call.arguments, result.is_some());
            let rendered = result.is_some_and(|result| {
                push_hashline_result(&mut body, &result.content, &changes, inner_width)
            });
            if !rendered {
                push_changes(&mut body, &changes, inner_width);
            }
        } else {
            push_edit_detail(
                &mut body,
                &call.arguments,
                result.map(|result| result.content.as_str()),
                inner_width,
                usize::MAX,
            );
        }
        let truncated = !full && body.len() > TOOL_PREVIEW_LIMIT;
        for line in body.into_iter().take(cap) {
            lines.push(fill_to_width(line.spans, max_width));
        }
        push_block_tail(lines, truncated, max_width, hint_style);
        return truncated;
    }

    if call.name == "Agent" && result.is_some_and(|result| result.task.is_some()) {
        let (body, _) = render_markdown(
            &result.expect("task result").content,
            max_width.saturating_sub(TOOL_BLOCK_CHROME),
        );
        let truncated = body.len() > cap;
        for line in body.into_iter().take(cap) {
            lines.push(fill_to_width(line.spans, max_width));
        }
        push_block_tail(lines, truncated, max_width, hint_style);
        return truncated;
    }
    let body = result.map(tool_result_rows).unwrap_or_default();
    let truncated = !full && body.len() > TOOL_PREVIEW_LIMIT;
    let shown = body.len().min(cap);
    for (text, style) in body.iter().take(shown) {
        push_filled_text(lines, text, *style, max_width);
    }
    push_block_tail(lines, truncated, max_width, hint_style);
    truncated
}

/// 展开块收尾：截断提示或留白，随后闭合底边框。
fn push_block_tail(
    lines: &mut Vec<Line<'static>>,
    truncated: bool,
    max_width: usize,
    hint_style: Style,
) {
    if truncated {
        push_filled_text(lines, "...", hint_style, max_width);
        push_filled_text(lines, "Click to expand", hint_style, max_width);
    } else {
        push_filled_blank(lines, max_width);
    }
    lines.push(tool_block_frame("╰", "╯", max_width));
}
