use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::harness::SessionView;
use crate::model::{
    Annotation, ContentPart, Role, ToolCall, ToolResult, WebSearchAction, WebSearchCall,
};
use crate::ui::app::{
    App, TextCoord, TextSelection, ThoughtHit, ToolExpand, ToolHit, char_in_column_range,
};
use crate::ui::markdown::{LinkRegion, render_markdown, wrap_spans_to_lines};
use crate::ui::text::truncate;
use crate::ui::views::header::session_title_line;
use crate::ui::views::{PANEL_BORDER, panel_block};

const SCROLLBAR_THUMB: &str = "│";
const SCROLLBAR_TRACK: &str = "│";

/// 聊天流正文可用宽度（右侧留出一列空白）
pub fn chat_text_width(area: Rect) -> usize {
    (area.width as usize).saturating_sub(1)
}

/// 用户消息引用块的底色与左侧色条颜色
const USER_BLOCK_BG: Color = Color::Rgb(28, 32, 40);
const USER_BLOCK_BAR: Color = Color::Rgb(96, 149, 239);
const SELECTION_STYLE: Style = Style::new().fg(Color::White).bg(Color::Rgb(60, 90, 140));

#[derive(Clone, Copy, Default)]
pub(crate) struct RenderState {
    prev_reasoning: bool,
    prev_text: bool,
    thought_groups: usize,
    thought_expanded: bool,
}

/// 构建用户消息引用块的单行：左侧色条起头，块底色填充至可用宽度
fn user_block_row(spans: Vec<Span<'static>>, max_width: usize) -> Line<'static> {
    let bar_style = Style::default().fg(USER_BLOCK_BAR).bg(USER_BLOCK_BG);
    let base = Style::default().bg(USER_BLOCK_BG);
    let mut width = 1;
    let mut out = vec![Span::styled("▌", bar_style)];
    for span in spans {
        width += span.content.width();
        out.push(span);
    }
    if max_width > width {
        out.push(Span::styled(" ".repeat(max_width - width), base));
    }
    Line::from(out)
}

/// 以左侧色条引用块渲染用户消息（含上下留白行），与模型输出形成区块级区分
fn push_user_block(lines: &mut Vec<Line<'static>>, text: &str, max_width: usize) {
    let text_style = Style::default()
        .fg(Color::Rgb(226, 229, 234))
        .bg(USER_BLOCK_BG);
    let inner_width = max_width.saturating_sub(3);
    lines.push(user_block_row(Vec::new(), max_width));
    for l in text.lines() {
        let spans = vec![Span::styled(l.to_string(), text_style)];
        for line in wrap_spans_to_lines(&spans, inner_width, "   ", "   ") {
            let mut spans = line.spans;
            if let Some(indent) = spans.first_mut() {
                indent.style = text_style;
            }
            lines.push(user_block_row(spans, max_width));
        }
    }
    lines.push(user_block_row(Vec::new(), max_width));
}

/// 从 URL 提取展示用域名（去协议、端口与 www. 前缀）
fn url_display_domain(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.split(':').next().unwrap_or(host);
    host.strip_prefix("www.").unwrap_or(host)
}

/// Collect cited URLs in first-appearance order within a response.
fn search_citations<'a>(
    parts: impl IntoIterator<Item = &'a ContentPart>,
) -> Vec<(&'a str, &'a str)> {
    let mut seen = HashSet::new();
    let mut citations = Vec::new();
    for part in parts {
        if let ContentPart::Text { annotations, .. } = part {
            for a in annotations {
                if let Annotation::UrlCitation { url, title, .. } = a
                    && seen.insert(url.as_str())
                {
                    citations.push((url.as_str(), title.as_str()));
                }
            }
        }
    }
    citations
}

fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

fn line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

fn glob_match_count(result: &ToolResult) -> Option<usize> {
    if result.is_error {
        return None;
    }
    Some(
        result
            .content
            .lines()
            .filter(|line| !line.is_empty() && *line != "[results truncated]")
            .count(),
    )
}

fn tool_call_detail(name: &str, arguments: &str, result: Option<&ToolResult>) -> String {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    match name {
        "Read" => json_str(&args, "path")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        "Edit" => match (
            json_str(&args, "path"),
            json_str(&args, "old_string"),
            json_str(&args, "new_string"),
        ) {
            (Some(path), Some(old_string), Some(new_string)) => format!(
                "{path} (+{}, -{})",
                line_count(new_string),
                line_count(old_string)
            ),
            _ => arguments.to_string(),
        },
        "Glob" => {
            let pattern = json_str(&args, "pattern")
                .map(|pattern| format!("\"{pattern}\""))
                .unwrap_or_else(|| arguments.to_string());
            match result.and_then(glob_match_count) {
                Some(count) => {
                    let noun = if count == 1 { "match" } else { "matches" };
                    format!("{pattern} ({count} {noun})")
                }
                None => pattern,
            }
        }
        "Grep" => {
            let Some(pattern) = json_str(&args, "pattern") else {
                return arguments.to_string();
            };
            match json_str(&args, "include") {
                Some(include) if !include.is_empty() => format!("{include}, {pattern}"),
                _ => pattern.to_string(),
            }
        }
        "Shell" => json_str(&args, "intent")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        _ => arguments.to_string(),
    }
}

fn tool_block_command(name: &str, arguments: &str) -> String {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    match name {
        "Shell" => json_str(&args, "command")
            .map(|command| format!("$ {command}"))
            .unwrap_or_else(|| arguments.to_string()),
        "Read" | "Edit" => json_str(&args, "path")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        "Glob" => json_str(&args, "pattern")
            .map(|pattern| format!("\"{pattern}\""))
            .unwrap_or_else(|| arguments.to_string()),
        "Grep" => json_str(&args, "pattern")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        _ => arguments.to_string(),
    }
}

pub(crate) struct ChatRender<'a> {
    pub lines: &'a mut Vec<Line<'static>>,
    pub links: &'a mut Vec<LinkRegion>,
    pub max_width: usize,
    pub thinking_concise: bool,
    pub results: &'a HashMap<String, &'a ToolResult>,
    pub expand: &'a HashMap<String, ToolExpand>,
    pub hits: &'a mut Vec<ToolHit>,
    pub thought_expand: &'a HashMap<String, bool>,
    pub thought_hits: &'a mut Vec<ThoughtHit>,
}

pub(crate) fn collect_tool_results(view: &SessionView) -> HashMap<String, &ToolResult> {
    let mut map = HashMap::new();
    let parts = view
        .session
        .turns
        .iter()
        .flat_map(|turn| turn.messages.iter().flat_map(|message| &message.parts))
        .chain(view.draft.iter());
    for part in parts {
        if let ContentPart::ToolResult(result) = part {
            map.insert(result.tool_call_id.clone(), result);
        }
    }
    map
}

const EDIT_DELETE: Style = Style::new().fg(Color::Rgb(220, 100, 100));
const EDIT_INSERT: Style = Style::new().fg(Color::Rgb(100, 180, 110));
const EDIT_DELETE_BG: Color = Color::Rgb(58, 37, 45);
const EDIT_INSERT_BG: Color = Color::Rgb(30, 50, 63);
const EDIT_SEP: Style = Style::new().fg(Color::DarkGray);
const EDIT_INDENT: usize = 3;
const EDIT_SEP_WIDTH: usize = 3;
const EDIT_MIN_COL: usize = 20;
const TOOL_BLOCK_BG: Color = Color::Rgb(40, 42, 46);
const TOOL_BLOCK_BORDER: Style = Style::new().fg(Color::Rgb(100, 100, 110)).bg(TOOL_BLOCK_BG);
const TOOL_PREVIEW_LIMIT: usize = 10;
const TOOL_BLOCK_CHROME: usize = 4;

fn tool_block_frame(left: &str, right: &str, max_width: usize) -> Line<'static> {
    let inner = max_width.saturating_sub(TOOL_BLOCK_CHROME);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(left.to_string(), TOOL_BLOCK_BORDER),
        Span::styled("─".repeat(inner), TOOL_BLOCK_BORDER),
        Span::styled(right.to_string(), TOOL_BLOCK_BORDER),
        Span::raw(" "),
    ])
}

fn fill_to_width(mut spans: Vec<Span<'static>>, max_width: usize) -> Line<'static> {
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

fn push_filled_blank(lines: &mut Vec<Line<'static>>, max_width: usize) {
    lines.push(fill_to_width(Vec::new(), max_width));
}

fn push_filled_text(lines: &mut Vec<Line<'static>>, text: &str, style: Style, max_width: usize) {
    let inner = max_width.saturating_sub(TOOL_BLOCK_CHROME).max(1);
    if text.is_empty() {
        push_filled_blank(lines, max_width);
        return;
    }
    let spans = vec![Span::styled(text.to_string(), style)];
    for wrapped in wrap_spans_to_lines(&spans, inner, " ", "  ") {
        lines.push(fill_to_width(wrapped.spans, max_width));
    }
}

fn tool_result_rows(result: &ToolResult) -> Vec<(String, Style)> {
    let style = if result.is_error {
        Style::default().fg(Color::Rgb(220, 120, 120))
    } else {
        Style::default().fg(Color::Rgb(180, 180, 180))
    };
    result
        .content
        .lines()
        .map(|line| (line.to_string(), style))
        .collect()
}

fn push_tool_block(
    lines: &mut Vec<Line<'static>>,
    call: &ToolCall,
    result: Option<&ToolResult>,
    full: bool,
    max_width: usize,
) -> bool {
    let cmd_style = Style::default().fg(Color::Rgb(210, 210, 210));
    let hint_style = Style::default().fg(Color::Rgb(110, 110, 110));
    lines.push(tool_block_frame("╭", "╮", max_width));
    push_filled_blank(lines, max_width);
    if call.name != "Edit" {
        push_filled_text(
            lines,
            &tool_block_command(&call.name, &call.arguments),
            cmd_style,
            max_width,
        );
        push_filled_blank(lines, max_width);
    }

    let cap = if full { usize::MAX } else { TOOL_PREVIEW_LIMIT };
    if call.name == "Edit" && !result.is_some_and(|result| result.is_error) {
        let mut body = Vec::new();
        push_edit_detail(
            &mut body,
            &call.arguments,
            result.map(|result| result.content.as_str()),
            max_width.saturating_sub(TOOL_BLOCK_CHROME),
            usize::MAX,
        );
        let truncated = !full && body.len() > TOOL_PREVIEW_LIMIT;
        for line in body.into_iter().take(cap) {
            lines.push(fill_to_width(line.spans, max_width));
        }
        if truncated {
            push_filled_text(lines, "...", hint_style, max_width);
            push_filled_text(lines, "Click to expand", hint_style, max_width);
        } else {
            push_filled_blank(lines, max_width);
        }
        lines.push(tool_block_frame("╰", "╯", max_width));
        return truncated;
    }

    let body = result.map(tool_result_rows).unwrap_or_default();
    let truncated = !full && body.len() > TOOL_PREVIEW_LIMIT;
    let shown = body.len().min(cap);
    for (text, style) in body.iter().take(shown) {
        push_filled_text(lines, text, *style, max_width);
    }
    if truncated {
        push_filled_text(lines, "...", hint_style, max_width);
        push_filled_text(lines, "Click to expand", hint_style, max_width);
    } else {
        push_filled_blank(lines, max_width);
    }
    lines.push(tool_block_frame("╰", "╯", max_width));
    truncated
}

fn push_tool_call(render: &mut ChatRender<'_>, call: &ToolCall) {
    let result = render.results.get(&call.id).copied();
    let expand = render.expand.get(&call.id).copied();
    let expanded = expand.is_some();
    let detail = tool_call_detail(&call.name, &call.arguments, result);
    let arrow = if expanded { "↓ " } else { "→ " };
    let name_style = if result.is_some_and(|result| result.is_error) {
        Style::default()
            .fg(Color::Rgb(220, 100, 100))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Rgb(210, 210, 210))
            .add_modifier(Modifier::BOLD)
    };
    let detail_style = Style::default().fg(Color::Rgb(150, 150, 150));
    let mut spans = vec![
        Span::styled(arrow, Style::default().fg(Color::Rgb(160, 160, 160))),
        Span::styled(call.name.clone(), name_style),
        Span::raw(" "),
    ];
    let edit_detail = serde_json::from_str::<serde_json::Value>(&call.arguments)
        .ok()
        .filter(|_| call.name == "Edit")
        .and_then(|args| {
            Some((
                json_str(&args, "path")?.to_string(),
                line_count(json_str(&args, "new_string")?),
                line_count(json_str(&args, "old_string")?),
            ))
        });
    if let Some((path, inserted, deleted)) = edit_detail {
        spans.extend([
            Span::styled(path, detail_style),
            Span::styled(" (", detail_style),
            Span::styled(format!("+{inserted}"), EDIT_INSERT),
            Span::styled(", ", detail_style),
            Span::styled(format!("-{deleted}"), EDIT_DELETE),
            Span::styled(")", detail_style),
        ]);
    } else {
        spans.push(Span::styled(detail, detail_style));
    }
    let header_start = render.lines.len();
    let wrapped = wrap_spans_to_lines(&spans, render.max_width, "", "   ");
    render.lines.extend(wrapped);
    let header_end = render.lines.len();
    let mut truncated = false;
    let mut block_start = render.lines.len();
    let mut block_end = block_start;
    if expanded {
        render.lines.push(Line::from(""));
        block_start = render.lines.len();
        truncated = push_tool_block(
            render.lines,
            call,
            result,
            expand == Some(ToolExpand::Full),
            render.max_width,
        );
        block_end = render.lines.len();
        render.lines.push(Line::from(""));
    }
    render.hits.push(ToolHit {
        id: call.id.clone(),
        header_start,
        header_end,
        block_start,
        block_end,
        truncated,
    });
}

enum EditRow<'a> {
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

fn edit_rows<'a>(
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
struct EditCell<'a> {
    line_number: Option<usize>,
    marker: &'static str,
    text: &'a str,
    style: Style,
    background: Option<Color>,
}

impl<'a> EditCell<'a> {
    fn new(line_number: usize, marker: &'static str, text: &'a str, style: Style) -> Self {
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

fn edit_side_lines(
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

fn push_edit_unified(
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

fn push_edit_numbered_row(lines: &mut Vec<Line<'static>>, cell: EditCell<'_>, max_width: usize) {
    lines.extend(edit_side_lines(cell, max_width, "   ", "     "));
}

fn push_edit_column_pair<'a>(
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

fn push_edit_change_block<'a>(
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

fn push_edit_columns<'a>(
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

fn edit_hunk_starts(diff: Option<&str>) -> Option<(usize, usize)> {
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

fn push_edit_detail(
    lines: &mut Vec<Line<'static>>,
    arguments: &str,
    result: Option<&str>,
    max_width: usize,
    cap: usize,
) {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return;
    };
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

fn call_covers_url(call: &WebSearchCall, url: &str) -> bool {
    match &call.action {
        WebSearchAction::Search => call.sources.iter().any(|source| source.url == url),
        WebSearchAction::OpenPage { url: action_url }
        | WebSearchAction::FindInPage {
            url: action_url, ..
        } => action_url == url,
    }
}

/// 轮次内统一汇总引用；思考分组状态延续到下一轮或草稿。
pub(crate) fn append_parts<'a>(
    render: &mut ChatRender<'_>,
    role: Role,
    parts: impl Iterator<Item = &'a ContentPart> + Clone,
    state: &mut RenderState,
) {
    let max_width = render.max_width;
    let thinking_concise = render.thinking_concise;
    let calls = parts
        .clone()
        .filter_map(|part| match part {
            ContentPart::WebSearchCall(call) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    let citations = search_citations(parts.clone());
    for part in parts {
        match (role, part) {
            (Role::User, ContentPart::Text { text, .. }) => {
                if let Some(reminder) = crate::system::parse_agents_reminder(text) {
                    match reminder.kind {
                        crate::system::AgentsReminderKind::Global
                        | crate::system::AgentsReminderKind::Project => {
                            // 全局与项目不展示
                        }
                        crate::system::AgentsReminderKind::Directory => {
                            // 按需自动加载的展示 Load xxx/AGENTS.md
                            let needs_separator = state.prev_reasoning || state.prev_text;
                            state.prev_reasoning = false;
                            state.prev_text = true;
                            if needs_separator {
                                render.lines.push(Line::from(""));
                            }
                            let spans = vec![Span::styled(
                                format!("Load {}", reminder.path),
                                Style::default().fg(Color::Rgb(150, 150, 150)),
                            )];
                            render
                                .lines
                                .extend(wrap_spans_to_lines(&spans, max_width, "", "   "));
                        }
                    }
                    continue;
                }
                state.prev_reasoning = false;
                state.prev_text = false;
                render.lines.push(Line::from(""));
                push_user_block(render.lines, text, max_width);
            }
            (Role::Assistant, ContentPart::Reasoning { content, .. }) => {
                // 连续思考段归为一段，组间保留空行。
                let group_start = !state.prev_reasoning;
                state.prev_reasoning = true;
                state.prev_text = false;
                if group_start {
                    render.lines.push(Line::from(""));
                }
                let body_style = Style::default()
                    .fg(Color::Rgb(120, 120, 120))
                    .add_modifier(Modifier::ITALIC);
                let prefix_style = Style::default()
                    .fg(Color::Rgb(140, 140, 140))
                    .add_modifier(Modifier::BOLD);

                if group_start {
                    state.thought_groups += 1;
                    state.thought_expanded = render
                        .thought_expand
                        .get(&format!("thought-{}", state.thought_groups))
                        .copied()
                        .unwrap_or(!thinking_concise);
                    let marker = if state.thought_expanded {
                        "- Thought"
                    } else {
                        "+ Thought"
                    };
                    let header_start = render.lines.len();
                    render
                        .lines
                        .push(Line::from(vec![Span::styled(marker, prefix_style)]));
                    render.thought_hits.push(ThoughtHit {
                        id: format!("thought-{}", state.thought_groups),
                        header_start,
                        header_end: header_start + 1,
                        body_start: header_start + 1,
                        body_end: header_start + 1,
                    });
                }
                if state.thought_expanded {
                    let (mut body, mut body_links) = render_markdown(content, max_width);
                    for link in &mut body_links {
                        link.line += render.lines.len();
                        link.columns.start = link.columns.start.saturating_sub(1);
                        link.columns.end = link.columns.end.saturating_sub(1);
                    }
                    render.links.extend(body_links);
                    for line in &mut body {
                        if let Some(span) = line.spans.first_mut()
                            && span.content.starts_with(' ')
                        {
                            span.content.to_mut().remove(0);
                        }
                        for span in &mut line.spans {
                            span.style = span.style.patch(body_style);
                        }
                    }
                    render.lines.extend(body);
                    // 当前分组的命中必为最新压入的一条；正文行持续累积，遇其他内容不再延伸
                    if let Some(hit) = render.thought_hits.last_mut() {
                        hit.body_end = render.lines.len();
                    }
                }
            }
            (Role::Assistant, ContentPart::ToolCall(call)) => {
                let needs_separator = state.prev_reasoning || state.prev_text;
                state.prev_reasoning = false;
                state.prev_text = false;
                if needs_separator {
                    render.lines.push(Line::from(""));
                }
                push_tool_call(render, call);
            }
            (Role::Tool, ContentPart::ToolResult(_)) => {
                state.prev_reasoning = false;
                state.prev_text = false;
            }
            (Role::Assistant, ContentPart::WebSearchCall(call)) => {
                let needs_separator = state.prev_reasoning || state.prev_text;
                state.prev_reasoning = false;
                state.prev_text = false;
                let (label, detail) = match &call.action {
                    WebSearchAction::Search => (
                        "web_search",
                        format!("{} ({} sources)", call.query, call.sources.len()),
                    ),
                    WebSearchAction::OpenPage { url } => ("web_open", url.clone()),
                    WebSearchAction::FindInPage { url, pattern } => {
                        ("web_find", format!("{url} / {pattern}"))
                    }
                };
                if needs_separator {
                    render.lines.push(Line::from(""));
                }
                let spans = vec![
                    Span::styled(
                        label,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(detail, Style::default().fg(Color::Rgb(160, 160, 160))),
                ];
                let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
                render.lines.extend(wrapped);

                if matches!(call.action, WebSearchAction::OpenPage { .. }) {
                    continue;
                }

                // Citation titles on the left, domains aligned to the right.
                for (url, title) in &citations {
                    if !call_covers_url(call, url) {
                        continue;
                    }
                    let domain = url_display_domain(url);
                    let title = title.trim();
                    let title = if title.is_empty() || title == *url {
                        domain
                    } else {
                        title
                    };
                    let domain = truncate(domain, max_width.saturating_sub(4) / 2);
                    let right_w = domain.width();
                    let title_max = max_width.saturating_sub(3 + right_w + 1);
                    let title = truncate(title, title_max);
                    let pad = title_max - title.width() + 1;
                    render.lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(title, Style::default().fg(Color::Rgb(170, 170, 170))),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(domain.to_string(), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            (Role::Assistant, ContentPart::Text { text, annotations }) => {
                state.prev_reasoning = false;
                state.prev_text = true;
                let citations: Vec<_> = annotations
                    .iter()
                    .filter_map(|a| {
                        if let Annotation::UrlCitation { title, url, .. } = a
                            && !calls.iter().any(|call| call_covers_url(call, url))
                        {
                            Some((title, url))
                        } else {
                            None
                        }
                    })
                    .collect();
                render.lines.push(Line::from(""));
                let (mut md_lines, mut md_links) = render_markdown(text, max_width);
                for link in &mut md_links {
                    link.line += render.lines.len();
                    link.columns.start = link.columns.start.saturating_sub(1);
                    link.columns.end = link.columns.end.saturating_sub(1);
                }
                for line in &mut md_lines {
                    if let Some(span) = line.spans.first_mut()
                        && span.content.starts_with(' ')
                    {
                        span.content.to_mut().remove(0);
                    }
                }
                render.links.extend(md_links);
                render.lines.extend(md_lines);

                // Citations not covered by a call remain below the text.
                if !citations.is_empty() {
                    render.lines.push(Line::from(""));
                    render.lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            "Sources / Citations:",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for (i, (title, url)) in citations.iter().enumerate() {
                        let spans = vec![
                            Span::styled(format!("[{}] ", i + 1), Style::default().fg(Color::Cyan)),
                            Span::styled((*title).clone(), Style::default().fg(Color::White)),
                            Span::raw(" - "),
                            Span::styled(
                                (*url).clone(),
                                Style::default().fg(Color::Rgb(100, 160, 255)),
                            ),
                        ];
                        let wrapped = wrap_spans_to_lines(&spans, max_width, "     ", "       ");
                        render.lines.extend(wrapped);
                    }
                }
            }
            // WebSearchCall 等未渲染的 part 不打断思考分组的连续性
            _ => {}
        }
    }
}

pub(crate) fn append_chat_tail(
    render: &mut ChatRender<'_>,
    view: &SessionView,
    notices: &[String],
    mut state: RenderState,
) {
    let is_empty_session = view.session.turns.iter().all(|t| {
        t.role == Role::System
            || (t.role == Role::User
                && t.messages.iter().all(|m| {
                    m.parts.iter().all(|p| match p {
                        ContentPart::Text { text, .. } => {
                            crate::system::parse_agents_reminder(text).is_some_and(|r| {
                                matches!(
                                    r.kind,
                                    crate::system::AgentsReminderKind::Global
                                        | crate::system::AgentsReminderKind::Project
                                )
                            })
                        }
                        _ => false,
                    })
                }))
    }) && view.draft.is_empty();

    if is_empty_session {
        render.lines.push(Line::from(""));
        render.lines.push(Line::from(vec![
            Span::styled(
                "  Start typing a prompt or use ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "/",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to access commands (/sessions to resume).",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    append_parts(render, Role::Assistant, view.draft.iter(), &mut state);
    for text in notices {
        render.lines.push(Line::from(""));
        let spans = vec![Span::styled(
            text.clone(),
            Style::default()
                .fg(Color::Rgb(200, 200, 200))
                .add_modifier(Modifier::ITALIC),
        )];
        let wrapped = wrap_spans_to_lines(&spans, render.max_width, "", "   ");
        render.lines.extend(wrapped);
    }
    if notices.is_empty() && state.prev_text {
        render.lines.push(Line::from(""));
    }
}

/// 用户消息块仅允许选择正文列，排除色条、缩进和填充。
fn selectable_columns(line: &Line) -> (usize, usize) {
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
pub fn apply_selection_highlight_offset(
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
            let mut cur_buf = String::new();
            let mut cur_is_selected: Option<bool> = None;

            for ch in span.content.chars() {
                let is_selected =
                    char_in_column_range(ch, col, range) && char_in_column_range(ch, col, bounds);

                match cur_is_selected {
                    Some(sel) if sel == is_selected => {
                        cur_buf.push(ch);
                    }
                    Some(sel) => {
                        let style = if sel { SELECTION_STYLE } else { span.style };
                        new_spans.push(Span::styled(std::mem::take(&mut cur_buf), style));
                        cur_buf.push(ch);
                        cur_is_selected = Some(is_selected);
                    }
                    None => {
                        cur_buf.push(ch);
                        cur_is_selected = Some(is_selected);
                    }
                }
                col += ch.width().unwrap_or(0);
            }

            if let Some(sel) = cur_is_selected
                && !cur_buf.is_empty()
            {
                let style = if sel { SELECTION_STYLE } else { span.style };
                new_spans.push(Span::styled(cur_buf, style));
            }
        }

        line.spans = new_spans;
    }
}

/// 在已绘制的可视行上覆盖选区底色，避免克隆行并重建 span
fn overlay_selection(
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

/// 对话与工具消息流视图（支持 Thinking 思考块、Tool 动作与结果、Assistant 最终回复 Markdown 渲染）
pub fn render_chat(app: &mut App, frame: &mut Frame, area: Rect) {
    let title = app.view.session.title.as_deref().unwrap_or("");
    let block = panel_block()
        .title(session_title_line(app, title))
        .padding(Padding::new(1, 1, 1, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let status = app.view.state.status();
    let show_status = !status.is_empty() && inner.height > 1;
    let viewport = if inner.width > 1 && inner.height > 0 {
        let status_height = if show_status { 2 } else { 0 };
        let height = inner.height.saturating_sub(status_height);
        Rect { height, ..inner }
    } else {
        area
    };
    app.chat.area = viewport;
    app.scrollbar_area = Rect::new(
        area.right().saturating_sub(1),
        viewport.y,
        u16::from(area.width > 0),
        viewport.height,
    );

    app.refresh_chat_lines(chat_text_width(viewport));
    let total_lines = app.chat.lines.len();
    let viewport_height = viewport.height as usize;
    let max_scroll = total_lines.saturating_sub(viewport_height);

    app.update_viewport(max_scroll, viewport_height);

    let scroll = app.chat_scroll();
    let start = scroll.min(total_lines);
    let end = (scroll + viewport_height).min(total_lines);
    let selection = app.chat.selection.get().copied();
    let text_width = viewport.width.saturating_sub(1);

    {
        let window = &app.chat.lines[start..end];
        for (i, line) in window.iter().enumerate() {
            frame.render_widget(
                line,
                Rect {
                    x: viewport.x,
                    y: viewport.y + i as u16,
                    width: text_width,
                    height: 1,
                },
            );
        }
        if let Some(sel) = selection {
            overlay_selection(frame.buffer_mut(), viewport, window, &sel, scroll);
        }
    }

    // 轨道与右边框重合，保留上下圆角。
    if max_scroll > 0 {
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll);

        let thumb_color = if app.is_dragging_scrollbar {
            Color::Rgb(150, 180, 225)
        } else {
            Color::Rgb(130, 140, 158)
        };

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol(SCROLLBAR_THUMB)
            .track_symbol(Some(SCROLLBAR_TRACK))
            .thumb_style(Style::default().fg(thumb_color))
            .track_style(Style::default().fg(PANEL_BORDER));

        frame.render_stateful_widget(scrollbar, app.scrollbar_area, &mut scrollbar_state);
    }

    if show_status {
        frame.render_widget(
            Line::from(Span::styled(
                format!("{} {status}", app.spinner_char()),
                Style::default().fg(Color::Yellow),
            )),
            Rect {
                x: inner.x,
                y: inner.bottom().saturating_sub(1),
                width: inner.width,
                height: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolCall, ToolResult, WebSearchSource};
    use crate::ui::app::{TextCoord, ToolExpand};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn joined(lines: &[Line]) -> String {
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn edit_detail_uses_unified_diff_when_narrow() {
        let mut lines = Vec::new();
        push_edit_detail(
            &mut lines,
            r#"{"path":"a.rs","old_string":"old","new_string":"new"}"#,
            None,
            40,
            20,
        );
        let text = joined(&lines);
        assert!(text.contains("   1 - old"));
        assert!(text.contains("   1 + new"));
        assert!(!text.contains("│"));
    }

    #[test]
    fn edit_detail_uses_columns_when_wide() {
        let mut lines = Vec::new();
        push_edit_detail(
            &mut lines,
            r#"{"path":"a.rs","old_string":"old","new_string":"new"}"#,
            None,
            80,
            20,
        );
        let text = joined(&lines);
        assert!(text.contains("   1 - old"));
        assert!(text.contains("   1 + new"));
        assert!(text.contains("│"));
    }

    #[test]
    fn edit_detail_uses_standard_line_backgrounds() {
        let mut lines = Vec::new();
        push_edit_detail(
            &mut lines,
            r#"{"path":"a.rs","old_string":"same\nold","new_string":"same\nnew"}"#,
            None,
            40,
            20,
        );

        let context = lines
            .iter()
            .find(|line| line.to_string().contains("same"))
            .unwrap();
        let deleted = lines
            .iter()
            .find(|line| line.to_string().contains("   2 - old"))
            .unwrap();
        let inserted = lines
            .iter()
            .find(|line| line.to_string().contains("   2 + new"))
            .unwrap();

        assert!(context.spans.iter().all(|span| span.style.bg.is_none()));
        assert!(
            deleted
                .spans
                .iter()
                .all(|span| span.style.bg == Some(EDIT_DELETE_BG))
        );
        assert!(
            inserted
                .spans
                .iter()
                .all(|span| span.style.bg == Some(EDIT_INSERT_BG))
        );
    }

    #[test]
    fn edit_detail_uses_hunk_line_numbers() {
        let mut lines = Vec::new();
        push_edit_detail(
            &mut lines,
            r#"{"path":"a.rs","old_string":"same\nold","new_string":"same\nnew"}"#,
            Some("--- a.rs\n+++ a.rs\n@@ -308,2 +308,2 @@\n same\n-old\n+new"),
            80,
            20,
        );

        let text = joined(&lines);
        assert!(text.contains(" 308   same"));
        assert!(text.contains(" 309 - old"));
        assert!(text.contains(" 309 + new"));
    }

    fn render_tools(
        parts: &[ContentPart],
        results: &HashMap<String, &ToolResult>,
        expand: &HashMap<String, ToolExpand>,
        width: usize,
    ) -> (Vec<Line<'static>>, Vec<ToolHit>) {
        let mut lines = Vec::new();
        let mut hits = Vec::new();
        let mut thought_hits = Vec::new();
        let mut state = RenderState::default();
        append_parts(
            &mut ChatRender {
                lines: &mut lines,
                links: &mut Vec::new(),
                max_width: width,
                thinking_concise: true,
                results,
                expand,
                hits: &mut hits,
                thought_expand: &HashMap::new(),
                thought_hits: &mut thought_hits,
            },
            Role::Assistant,
            parts.iter(),
            &mut state,
        );
        (lines, hits)
    }

    #[test]
    fn tool_call_collapsed_is_one_line() {
        let call = ToolCall::new("c1", "Read", r#"{"path":"README.md"}"#);
        let result = ToolResult::success("c1", "1: hello\n2: world");
        let mut results = HashMap::new();
        results.insert(result.tool_call_id.clone(), &result);
        let (lines, _) = render_tools(
            &[ContentPart::ToolCall(call)],
            &results,
            &HashMap::new(),
            80,
        );
        let text = joined(&lines);
        assert!(text.contains("→ Read README.md"));
        assert!(!text.contains("hello"));
        assert!(!text.contains("Output:"));
    }

    #[test]
    fn edit_header_colors_counts_and_block_omits_path() {
        let call = ToolCall::new(
            "c1",
            "Edit",
            r#"{"path":"src/main.rs","old_string":"old\nline","new_string":"new\nlines\nhere"}"#,
        );
        let result = ToolResult::success("c1", "Done");
        let mut results = HashMap::new();
        results.insert(result.tool_call_id.clone(), &result);
        let mut expand = HashMap::new();
        expand.insert("c1".into(), ToolExpand::Preview);

        let (lines, _) = render_tools(&[ContentPart::ToolCall(call)], &results, &expand, 80);

        assert_eq!(joined(&lines).matches("src/main.rs").count(), 1);
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| { span.content == "+3" && span.style.fg == EDIT_INSERT.fg })
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| { span.content == "-2" && span.style.fg == EDIT_DELETE.fg })
        );
    }

    #[test]
    fn tool_glob_shows_match_count() {
        let call = ToolCall::new("c1", "Glob", r#"{"pattern":"**/*.rs"}"#);
        let result = ToolResult::success("c1", "a.rs\nb.rs\nc.rs");
        let mut results = HashMap::new();
        results.insert(result.tool_call_id.clone(), &result);
        let (lines, _) = render_tools(
            &[ContentPart::ToolCall(call)],
            &results,
            &HashMap::new(),
            80,
        );
        assert!(joined(&lines).contains("→ Glob \"**/*.rs\" (3 matches)"));
    }

    #[test]
    fn tool_shell_collapsed_shows_intent() {
        let call = ToolCall::new(
            "c1",
            "Shell",
            r#"{"intent":"count lines","command":"wc -l src/*.ts"}"#,
        );
        let result = ToolResult::success("c1", "26 src/result.ts\n30 src/types.ts");
        let mut results = HashMap::new();
        results.insert(result.tool_call_id.clone(), &result);
        let (lines, _) = render_tools(
            &[ContentPart::ToolCall(call)],
            &results,
            &HashMap::new(),
            80,
        );
        let text = joined(&lines);
        assert!(text.contains("→ Shell count lines"));
        assert!(!text.contains("wc -l"));
        assert!(!text.contains("src/result.ts"));
    }

    #[test]
    fn tool_preview_block_truncates_and_full_shows_all() {
        let output: String = (1..=15)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let call = ToolCall::new("c1", "Shell", r#"{"intent":"list","command":"seq 15"}"#);
        let result = ToolResult::success("c1", output);
        let mut results = HashMap::new();
        results.insert(result.tool_call_id.clone(), &result);
        let mut expand = HashMap::new();
        expand.insert("c1".into(), ToolExpand::Preview);
        let (lines, hits) = render_tools(
            &[ContentPart::ToolCall(call.clone())],
            &results,
            &expand,
            80,
        );
        let text = joined(&lines);
        assert!(text.contains("↓ Shell list"));
        assert!(text.contains("$ seq 15"));
        assert!(text.contains("line1"));
        assert!(text.contains("line10"));
        assert!(!text.contains("line11"));
        assert!(text.contains("Click to expand"));
        assert_eq!(hits.len(), 1);
        assert!(hits[0].truncated);

        expand.insert("c1".into(), ToolExpand::Full);
        let (lines, hits) = render_tools(&[ContentPart::ToolCall(call)], &results, &expand, 80);
        let text = joined(&lines);
        assert!(text.contains("line15"));
        assert!(!text.contains("Click to expand"));
        assert!(!hits[0].truncated);
    }

    #[test]
    fn test_user_block_copy_excludes_decoration() {
        let mut lines = Vec::new();
        push_user_block(
            &mut lines,
            "第一行示例 https://example.com\n第二行混合文本",
            80,
        );
        let start = TextCoord { line: 0, col: 0 };
        let end = TextCoord {
            line: lines.len() - 1,
            col: 80,
        };
        for selection in [
            TextSelection::new(start, end),
            TextSelection::new(end, start),
        ] {
            assert_eq!(
                extract_selected_text(&lines, &selection).as_deref(),
                Some("第一行示例 https://example.com\n第二行混合文本")
            );
        }
        let selection =
            TextSelection::new(TextCoord { line: 1, col: 0 }, TextCoord { line: 1, col: 4 });
        assert_eq!(
            extract_selected_text(&lines, &selection).as_deref(),
            Some("")
        );
    }

    fn build_chat_lines(app: &App, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut state = RenderState::default();
        let results = collect_tool_results(&app.view);
        let mut hits = Vec::new();
        let mut thought_hits = Vec::new();
        for turn in &app.view.session.turns {
            append_parts(
                &mut ChatRender {
                    lines: &mut lines,
                    links: &mut Vec::new(),
                    max_width: width.max(20),
                    thinking_concise: app.thinking_concise,
                    results: &results,
                    expand: &app.tool_expand,
                    hits: &mut hits,
                    thought_expand: &app.thought_expand,
                    thought_hits: &mut thought_hits,
                },
                turn.role,
                turn.messages.iter().flat_map(|m| &m.parts),
                &mut state,
            );
        }
        append_chat_tail(
            &mut ChatRender {
                lines: &mut lines,
                links: &mut Vec::new(),
                max_width: width.max(20),
                thinking_concise: app.thinking_concise,
                results: &results,
                expand: &app.tool_expand,
                hits: &mut hits,
                thought_expand: &app.thought_expand,
                thought_hits: &mut thought_hits,
            },
            &app.view,
            &app.notices,
            state,
        );
        lines
    }

    async fn create_test_app() -> App {
        crate::ui::test_support::create_test_app("init_model", None, None, false)
            .await
            .app
    }

    #[tokio::test]
    async fn test_chat_empty_placeholder() {
        let app = create_test_app().await;
        let lines = build_chat_lines(&app, 80);
        assert!(lines.len() >= 2);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(joined.contains("Start typing a prompt"));
    }

    #[tokio::test]
    async fn test_chat_cache_preserves_history_and_layout() {
        let mut ta =
            crate::ui::test_support::create_test_app("init_model", None, None, false).await;
        let app = &mut ta.app;
        let check = |app: &mut App, width| {
            let expected = build_chat_lines(app, width);
            app.refresh_chat_lines(width);
            assert_eq!(app.chat.lines, expected);
        };
        check(app, 80);
        app.add_part(
            Role::User,
            ContentPart::text("History 中文与 English words ".repeat(4)),
        );
        app.add_part(
            Role::Assistant,
            ContentPart::reasoning("**First thought**", None),
        );
        check(app, 80);
        let history_text = |app: &App| {
            app.chat
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.contains("History"))
                .unwrap()
                .content
                .as_ptr()
        };
        let history = history_text(app);
        for draft in [
            vec![ContentPart::reasoning("**Next thought**", None)],
            vec![
                ContentPart::reasoning("**Next thought**", None),
                ContentPart::text("**Answer**"),
            ],
            vec![],
        ] {
            let view = Arc::make_mut(&mut app.view);
            view.revision += 1;
            view.draft = draft;
            check(app, 80);
            assert_eq!(history_text(app), history);
        }
        app.notices.push("Local notice".into());
        check(app, 80);
        app.notices.clear();
        check(app, 80);
        assert_eq!(history_text(app), history);

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft
            .push(ContentPart::reasoning("**Next thought**", None));
        for concise in [false, true] {
            app.thinking_concise = concise;
            for width in [30, 120, 80] {
                check(app, width);
            }
        }
        // 会话内容替换时，即使轮次数与视图版本相同也必须重建历史。
        let session = Arc::make_mut(&mut Arc::make_mut(&mut app.view).session);
        session.turns[0] =
            crate::model::Turn::user_turn(crate::model::Message::text("Replacement"));
        check(app, 80);
        app.attach_session(app.active_session.clone());
        check(app, 80);
        assert!(
            app.chat
                .lines
                .iter()
                .any(|line| line.to_string().contains("Start typing"))
        );
        std::fs::remove_dir_all(ta.temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_thinking_grouping_and_concise_modes() {
        let mut app = create_test_app().await;
        app.thinking_concise = false;
        // 模拟协议层输出：各 summary part 带原始 markdown 标记，以空行（段落边界）分隔
        app.add_part(
            Role::Assistant,
            ContentPart::reasoning(
                "**Collecting example sources**\n\n**Drafting example response**".to_string(),
                None,
            ),
        );

        // Full mode keeps all paragraphs under one heading.
        let lines = build_chat_lines(&app, 80);
        let thought_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.spans.first().is_some_and(|s| s.content == "- Thought"))
            .collect();
        assert_eq!(thought_lines.len(), 1);
        assert_eq!(thought_lines[0].to_string(), "- Thought");
        assert!(
            !thought_lines[0]
                .spans
                .first()
                .unwrap()
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        let first_body = lines
            .iter()
            .find(|l| l.to_string().contains("Collecting"))
            .unwrap();
        assert_eq!(first_body.to_string(), "  Collecting example sources");

        // markdown 标记被渲染掉，加粗修饰保留，两段分行
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Collecting example sources"));
        assert!(all_text.contains("Drafting example response"));
        assert!(!all_text.contains("**"));
        let bold_in_first = first_body.spans.iter().any(|s| {
            s.content.contains("Collecting") && s.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(bold_in_first);

        // 追加搜索调用与第二个思考 part：搜索块渲染为分组边界，前后思考各自成段。
        // 搜索调用、思考与带注脚的正文同属一轮，来源由同轮注脚提供。
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
            Role::Assistant,
            vec![
                crate::model::Message::new(vec![ContentPart::WebSearchCall(
                    WebSearchCall::completed(
                        "ws_1",
                        "示例查询",
                        vec![WebSearchSource::new(
                            "https://source.example/article",
                            "Uncited candidate",
                            None,
                        )],
                    ),
                )]),
                crate::model::Message::new(vec![ContentPart::reasoning(
                    "**Cross-checking sources**".to_string(),
                    None,
                )]),
                crate::model::Message::new(vec![ContentPart::Text {
                    text: "这里是带引用的示例答复。".to_string(),
                    annotations: vec![Annotation::UrlCitation {
                        title: "示例来源".into(),
                        url: "https://source.example/article".into(),
                        start_index: 0,
                        end_index: 1,
                    }],
                }]),
            ],
        ));
        let lines = build_chat_lines(&app, 80);
        let header_idx: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.spans.first().is_some_and(|s| s.content == "- Thought"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(header_idx.len(), 2);
        // 搜索块出现在两组思考之间，来源行左侧为注脚补齐的网页标题、域名右对齐至可用宽度
        let search_idx = lines
            .iter()
            .position(|l| l.spans.iter().any(|s| s.content == "web_search"))
            .expect("web search block rendered");
        assert!(search_idx > header_idx[0] && search_idx < header_idx[1]);
        let source_row = &lines[search_idx + 1];
        assert!(
            source_row
                .spans
                .iter()
                .any(|s| s.content.contains("示例来源"))
        );
        let last = source_row.spans.last().unwrap();
        assert_eq!(last.content, "source.example");
        let row_width: usize = source_row.spans.iter().map(|s| s.content.width()).sum();
        assert_eq!(row_width, 80);

        // 轮次内已有搜索调用：正文后的末尾来源汇总不再渲染
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("这里是带引用的示例答复"));
        assert!(all_text.contains("示例查询 (1 sources)"));
        assert!(!all_text.contains("candidate"));
        assert!(!all_text.contains("Sources / Citations"));

        // concise 模式：搜索块两侧的思考各自合并为单行标记，段落内容隐藏
        app.thinking_concise = true;
        let lines = build_chat_lines(&app, 80);
        let headers = lines
            .iter()
            .filter(|l| l.spans.first().is_some_and(|s| s.content == "+ Thought"))
            .count();
        assert_eq!(headers, 2);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(!all_text.contains("Collecting example sources"));
        assert!(!all_text.contains("**"));
    }

    #[tokio::test]
    async fn test_thought_group_click_toggles_individual_group() {
        let mut app = create_test_app().await;
        app.add_part(
            Role::Assistant,
            ContentPart::reasoning("First group body", None),
        );
        app.add_part(Role::Assistant, ContentPart::text("Middle answer"));
        app.add_part(
            Role::Assistant,
            ContentPart::reasoning("Second group body", None),
        );

        app.refresh_chat_lines(80);
        let headers: Vec<(String, usize)> = app
            .chat_cache
            .thought_hits
            .iter()
            .map(|hit| (hit.id.clone(), hit.header_start))
            .collect();
        assert_eq!(
            headers
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["thought-1", "thought-2"]
        );
        let text = |app: &App| -> String {
            app.chat
                .lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
                .collect()
        };
        assert!(!text(&app).contains("First group body"));
        assert!(!text(&app).contains("Second group body"));

        // 展开第一组：仅该组正文可见，标题切换为展开标记
        assert!(app.handle_thought_click(headers[0].1));
        app.refresh_chat_lines(80);
        let rendered = text(&app);
        assert!(rendered.contains("First group body"));
        assert!(!rendered.contains("Second group body"));
        assert!(
            app.chat
                .lines
                .iter()
                .any(|l| { l.spans.first().is_some_and(|s| s.content == "- Thought") })
        );

        // 点击正文区域折叠：覆盖值回到全局默认时移除条目
        let first = app.chat_cache.thought_hits[0].clone();
        assert!(first.body_end > first.body_start);
        assert!(app.handle_thought_click(first.body_start));
        app.refresh_chat_lines(80);
        assert!(!text(&app).contains("First group body"));
        assert!(app.thought_expand.is_empty());

        // 全局切换到 Full 清空既有覆盖，两组正文均可见
        assert!(app.handle_thought_click(headers[1].1));
        app.toggle_thinking_display();
        assert!(app.thought_expand.is_empty());
        app.refresh_chat_lines(80);
        let rendered = text(&app);
        assert!(rendered.contains("First group body"));
        assert!(rendered.contains("Second group body"));
    }

    #[tokio::test]
    async fn test_separate_search_calls_track_draft_citations_and_turn_boundaries() {
        let mut app = create_test_app().await;
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft = vec![
            ContentPart::text("Before search"),
            ContentPart::WebSearchCall(WebSearchCall::completed(
                "ws_1",
                "query A; query A2",
                vec![
                    WebSearchSource::new(
                        "https://candidate.example/page",
                        "Uncited candidate",
                        None,
                    ),
                    WebSearchSource::new(
                        "https://www.cited.example/page",
                        "Source title must not appear",
                        None,
                    ),
                ],
            )),
            ContentPart::text("Between searches"),
            ContentPart::WebSearchCall(WebSearchCall::completed(
                "ws_2",
                "query B",
                vec![
                    WebSearchSource::new("https://www.cited.example/page", "Ignored", None),
                    WebSearchSource::new("https://other.example/page", "Ignored", None),
                ],
            )),
        ];
        app.refresh_chat_lines(100);
        let text = |lines: &[Line<'_>]| -> String {
            lines
                .iter()
                .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
                .collect()
        };
        let initial = text(&app.chat.lines);
        assert_eq!(initial.matches("web_search").count(), 2);
        assert!(initial.contains("web_search query A; query A2 (2 sources)"));
        assert!(initial.contains("web_search query B (2 sources)"));
        assert!(!initial.contains("candidate"));
        assert!(initial.find("Before search").unwrap() < initial.find("web_search").unwrap());
        assert!(initial.find("web_search").unwrap() < initial.find("Between searches").unwrap());
        assert!(initial.find("Between searches").unwrap() < initial.find("query B").unwrap());

        let citation = Annotation::UrlCitation {
            url: "https://www.cited.example/page".into(),
            title: "Provided title".into(),
            start_index: 0,
            end_index: 1,
        };
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft.push(ContentPart::Text {
            text: "Answer".into(),
            annotations: vec![citation.clone()],
        });
        app.refresh_chat_lines(100);
        let updated = text(&app.chat.lines);
        assert!(updated.contains("query A; query A2 (2 sources)"));
        assert!(updated.contains("query B (2 sources)"));
        assert_eq!(updated.matches("Provided title").count(), 2);
        assert!(updated.contains("cited.example"));
        assert!(!updated.contains("candidate"));

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft.push(ContentPart::Text {
            text: "More".into(),
            annotations: vec![
                citation,
                Annotation::UrlCitation {
                    url: "https://other.example/page".into(),
                    title: String::new(),
                    start_index: 0,
                    end_index: 1,
                },
                Annotation::UrlCitation {
                    url: "https://unmatched.example/page".into(),
                    title: "Unmatched reference".into(),
                    start_index: 0,
                    end_index: 1,
                },
            ],
        });
        app.refresh_chat_lines(100);
        let rendered = text(&app.chat.lines);
        assert_eq!(rendered.matches("web_search").count(), 2);
        assert!(rendered.contains("query A; query A2 (2 sources)"));
        assert!(rendered.contains("query B (2 sources)"));
        assert_eq!(rendered.matches("Provided title").count(), 2);
        let first_group = rendered.split("Between searches").next().unwrap();
        assert!(first_group.contains("Provided title"));
        assert!(!first_group.contains("other.example"));
        let second_group = rendered.split("query B (2 sources)").nth(1).unwrap();
        assert!(second_group.contains("Provided title"));
        assert!(second_group.contains("other.example"));
        assert!(!rendered.contains("Source title must not appear"));
        assert!(!rendered.contains("Ignored"));
        let listing = rendered.split("Sources / Citations:").nth(1).unwrap();
        assert!(listing.contains("Unmatched reference - https://unmatched.example/page"));
        assert!(!listing.contains("Provided title"));
        assert!(!listing.contains("other.example"));

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        let messages = std::mem::take(&mut view.draft)
            .into_iter()
            .map(|part| crate::model::Message::new(vec![part]))
            .collect();
        Arc::make_mut(&mut view.session)
            .append_turn(crate::model::Turn::new(Role::Assistant, messages));
        app.refresh_chat_lines(100);
        assert_eq!(text(&app.chat.lines), rendered);

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft
            .push(ContentPart::WebSearchCall(WebSearchCall::completed(
                "ws_3",
                "query C",
                vec![WebSearchSource::new(
                    "https://www.cited.example/page",
                    "Ignored",
                    None,
                )],
            )));
        app.refresh_chat_lines(100);
        let separate = text(&app.chat.lines);
        assert_eq!(separate.matches("web_search").count(), 3);
        assert!(separate.contains("query A; query A2 (2 sources)"));
        assert!(separate.contains("query C (1 sources)"));
        assert_eq!(separate.matches("Provided title").count(), 2);
        assert!(
            !separate
                .split("query C (1 sources)")
                .nth(1)
                .unwrap()
                .contains("Provided title")
        );
    }

    #[tokio::test]
    async fn test_open_and_find_match_action_urls() {
        let mut app = create_test_app().await;
        let url = "https://www.example.com/page";
        let mut open = WebSearchCall::completed("open", "unused query", vec![]);
        open.action = WebSearchAction::OpenPage { url: url.into() };
        let mut find = WebSearchCall::completed("find", "unused query", vec![]);
        find.action = WebSearchAction::FindInPage {
            url: url.into(),
            pattern: "matching words".into(),
        };
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft = vec![
            ContentPart::WebSearchCall(open),
            ContentPart::text("Between actions"),
            ContentPart::WebSearchCall(find),
            ContentPart::Text {
                text: "Answer".into(),
                annotations: vec![
                    Annotation::UrlCitation {
                        url: url.into(),
                        title: "Annotation title".into(),
                        start_index: 0,
                        end_index: 1,
                    },
                    Annotation::UrlCitation {
                        url: "https://unmatched.example/page".into(),
                        title: "Unmatched title".into(),
                        start_index: 0,
                        end_index: 1,
                    },
                ],
            },
        ];
        let lines = build_chat_lines(&app, 100);
        let text: String = lines.iter().map(|line| line.to_string()).collect();
        assert!(text.contains(&format!("web_open {url}")));
        assert!(text.contains(&format!("web_find {url} / matching words")));
        assert!(!text.contains("sources)"));
        assert!(!text.contains("unused query"));
        assert_eq!(text.matches("Annotation title").count(), 1);
        let open_group = text.split("Between actions").next().unwrap();
        assert!(!open_group.contains("Annotation title"));
        assert!(!open_group.contains("Unmatched title"));
        let find_group = text
            .split("web_find")
            .nth(1)
            .unwrap()
            .split("Answer")
            .next()
            .unwrap();
        assert!(find_group.contains("Annotation title"));
        assert!(!find_group.contains("Unmatched title"));
        let listing = text.split("Sources / Citations:").nth(1).unwrap();
        assert!(listing.contains("Unmatched title"));
        assert!(!listing.contains("Annotation title"));
        for width in [20, 30] {
            let narrow = build_chat_lines(&app, width);
            let source_rows: Vec<_> = narrow
                .iter()
                .filter(|line| {
                    line.spans.len() == 4
                        && line.spans.last().unwrap().style.fg == Some(Color::DarkGray)
                })
                .collect();
            assert_eq!(source_rows.len(), 1);
            for line in source_rows {
                assert_eq!(line.width(), width);
            }
        }
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        let parts = std::mem::take(&mut view.draft);
        Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
            Role::Assistant,
            vec![crate::model::Message::new(parts)],
        ));
        assert_eq!(build_chat_lines(&app, 100), lines);
    }

    #[tokio::test]
    async fn test_user_message_block_rendering() {
        let mut app = create_test_app().await;
        app.add_part(Role::User, ContentPart::text("hello"));
        let lines = build_chat_lines(&app, 80);
        let block_rows: Vec<_> = lines
            .iter()
            .filter(|l| l.spans.first().is_some_and(|s| s.content == "▌"))
            .collect();
        // 上下留白行与内容行均以色条起头，整行填充块底色至可用宽度
        assert!(block_rows.len() >= 3);
        for row in &block_rows {
            let width: usize = row.spans.iter().map(|s| s.content.width()).sum();
            assert_eq!(width, 80);
            assert!(row.spans.iter().any(|s| s.style.bg == Some(USER_BLOCK_BG)));
        }
    }

    #[tokio::test]
    async fn test_assistant_markdown_rendering() {
        let mut app = create_test_app().await;
        let md_content = r#"# Hello World
This is **bold** text and `inline code`.

Here is a list:
- Item 1
- Item 2

```rust
fn main() {
    println!("hello");
}
```
"#;
        app.add_part(
            Role::Assistant,
            ContentPart::Text {
                text: md_content.to_string(),
                annotations: vec![Annotation::UrlCitation {
                    title: "Rust Lang".into(),
                    url: "https://rust-lang.org".into(),
                    start_index: 0,
                    end_index: 1,
                }],
            },
        );

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());

        // 检查渲染出的 Markdown 内容
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Hello World"));
        assert!(all_text.contains("bold"));
        assert!(all_text.contains("inline code"));
        assert!(all_text.contains("Item 1"));
        assert!(all_text.contains("Item 2"));
        assert!(all_text.contains("println!"));
        assert!(all_text.contains("Rust Lang"));
        assert!(all_text.contains("https://rust-lang.org"));

        // 检查非空内容行均与 thinking 正文保持相同前缀缩进
        let assistant_lines: Vec<_> = lines
            .iter()
            .filter(|l| {
                l.spans
                    .iter()
                    .any(|s| s.content.contains("Hello World") || s.content.contains("bold"))
            })
            .collect();
        for line in assistant_lines {
            assert_eq!(line.spans.first().map(|s| s.content.as_ref()), Some("  "));
        }
    }

    #[tokio::test]
    async fn test_assistant_markdown_streaming_and_complex_elements() {
        let mut app = create_test_app().await;
        let md_streaming = r#"> Note: this is a blockquote

| Column A | Column B |
| -------- | -------- |
| Value 1  | Value 2  |

```python
def stream_test():
    pass
"#;
        app.add_part(
            Role::Assistant,
            ContentPart::Text {
                text: md_streaming.to_string(),
                annotations: Vec::new(),
            },
        );

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Note: this is a blockquote"));
        assert!(all_text.contains("Column A"));
        assert!(all_text.contains("Value 1"));
        assert!(all_text.contains("def stream_test():"));
    }

    #[tokio::test]
    async fn test_assistant_markdown_empty() {
        let mut app = create_test_app().await;
        app.add_part(
            Role::Assistant,
            ContentPart::Text {
                text: String::new(),
                annotations: Vec::new(),
            },
        );

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());
    }

    #[tokio::test]
    async fn test_render_chat_scrollbar_and_scrolling() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = create_test_app().await;
        // 添加大量文本使得行数超过视口（高度为 10）
        for i in 0..30 {
            app.add_part(Role::User, ContentPart::text(format!("Message line {i}")));
        }

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_chat(&mut app, f, f.area());
            })
            .unwrap();

        // 验证视口和滚动状态已自动计算，并且处于底部
        let viewport = Rect::new(2, 2, 76, 7);
        assert!(app.max_scroll > 0);
        assert_eq!(app.viewport_height, 7);
        assert!(app.auto_scroll);
        assert_eq!(app.scroll_offset, app.max_scroll);
        assert_eq!(app.chat.area, viewport);

        // 验证滚到底时，滚动条滑块位于视口最底部行
        let bar_x = 79;
        assert_eq!(
            app.scrollbar_area,
            Rect::new(bar_x, viewport.y, 1, viewport.height)
        );
        let buffer = terminal.backend().buffer();
        let last_cell = buffer
            .cell((bar_x, viewport.y + viewport.height - 1))
            .unwrap();
        assert_eq!(last_cell.symbol(), SCROLLBAR_THUMB);
        assert_eq!(last_cell.fg, Color::Rgb(130, 140, 158));
        assert_eq!(buffer.cell((bar_x, viewport.y)).unwrap().fg, PANEL_BORDER);
        assert_eq!(buffer.cell((77, viewport.y)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((bar_x, 9)).unwrap().symbol(), "╯");

        // 滚到顶时，滚动条滑块位于视口最顶部行
        app.scroll_to_top();
        terminal
            .draw(|f| {
                render_chat(&mut app, f, f.area());
            })
            .unwrap();
        let top_cell = terminal
            .backend()
            .buffer()
            .cell((bar_x, viewport.y))
            .unwrap();
        assert_eq!(top_cell.symbol(), SCROLLBAR_THUMB);
        assert_eq!(top_cell.fg, Color::Rgb(130, 140, 158));
    }

    #[tokio::test]
    async fn test_render_chat_selection_overlay() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = create_test_app().await;
        app.add_part(Role::User, ContentPart::text("hello world"));

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_chat(&mut app, f, f.area());
            })
            .unwrap();

        let line_idx = app
            .chat
            .lines
            .iter()
            .position(|line| line.spans.iter().any(|span| span.content.contains("hello")))
            .expect("user message rendered");
        app.chat.selection.start(line_idx, 0);
        app.chat.selection.update(line_idx, 5);
        app.chat.selection.finish();

        terminal
            .draw(|f| {
                render_chat(&mut app, f, f.area());
            })
            .unwrap();

        let scroll = app.chat_scroll();
        let area = app.chat.area;
        let row = area.y + (line_idx.saturating_sub(scroll)) as u16;
        let cell = terminal.backend().buffer().cell((area.x, row)).unwrap();
        assert_eq!(cell.bg, USER_BLOCK_BG);
        let text_cell = terminal.backend().buffer().cell((area.x + 4, row)).unwrap();
        assert_eq!(text_cell.bg, Color::Rgb(60, 90, 140));
        let outside = terminal.backend().buffer().cell((area.x + 6, row)).unwrap();
        assert_ne!(outside.bg, Color::Rgb(60, 90, 140));
    }

    #[test]
    fn test_highlight_offset_window_above_selection_start() {
        // 选区从窗口上方开始：窗口首行应整行高亮，而非从原起始列开始
        let mut window = vec![
            Line::from(vec![Span::raw("second")]),
            Line::from(vec![Span::raw("third")]),
        ];
        let sel = TextSelection::new(TextCoord { line: 0, col: 3 }, TextCoord { line: 2, col: 2 });
        apply_selection_highlight_offset(&mut window, &sel, 1);
        let first_line_selected: String = window[0]
            .spans
            .iter()
            .filter(|span| span.style.bg.is_some())
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(first_line_selected, "second");
    }

    #[test]
    fn test_extract_selected_text_single_line() {
        let lines = vec![Line::from(vec![
            Span::raw("Hello "),
            Span::styled("World", Style::default().fg(Color::Green)),
            Span::raw("!"),
        ])];
        let sel = TextSelection::new(
            TextCoord { line: 0, col: 6 },
            TextCoord { line: 0, col: 11 },
        );
        let extracted = extract_selected_text(&lines, &sel);
        assert_eq!(extracted, Some("World".to_string()));
    }

    #[test]
    fn test_extract_selected_text_multiline_cjk() {
        let lines = vec![
            Line::from(vec![Span::raw("第一行：你好世界")]),
            Line::from(vec![Span::raw("第二行：Rust编程语言")]),
        ];
        // 第一行 "第一行：" 宽度为 4*2=8 -> col 8 开始选取 "你好世界"
        // 第二行 "第二行：" 宽度为 8，"Rust" 宽度为 4 -> col 12 结束选取
        let sel = TextSelection::new(
            TextCoord { line: 0, col: 8 },
            TextCoord { line: 1, col: 12 },
        );
        let extracted = extract_selected_text(&lines, &sel);
        assert_eq!(extracted, Some("你好世界\n第二行：Rust".to_string()));
    }

    #[test]
    fn test_extract_selected_text_reverse_drag() {
        let lines = vec![
            Line::from(vec![Span::raw("Line 0 alpha")]),
            Line::from(vec![Span::raw("Line 1 beta")]),
        ];
        let sel = TextSelection::new(
            TextCoord { line: 1, col: 11 },
            TextCoord { line: 0, col: 7 },
        );
        let extracted = extract_selected_text(&lines, &sel);
        assert_eq!(extracted, Some("alpha\nLine 1 beta".to_string()));
    }

    #[test]
    fn test_apply_selection_highlight() {
        let mut lines = vec![Line::from(vec![
            Span::styled("Prefix ", Style::default().fg(Color::Yellow)),
            Span::styled("Selected", Style::default().fg(Color::Green)),
            Span::styled(" Suffix", Style::default().fg(Color::Red)),
        ])];
        let sel = TextSelection::new(
            TextCoord { line: 0, col: 7 },
            TextCoord { line: 0, col: 15 },
        );
        apply_selection_highlight(&mut lines, &sel);

        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(lines[0].spans[0].content, "Prefix ");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Yellow));

        assert_eq!(lines[0].spans[1].content, "Selected");
        assert_eq!(lines[0].spans[1].style.bg, Some(Color::Rgb(60, 90, 140)));
        assert_eq!(lines[0].spans[1].style.fg, Some(Color::White));

        assert_eq!(lines[0].spans[2].content, " Suffix");
        assert_eq!(lines[0].spans[2].style.fg, Some(Color::Red));
    }

    #[test]
    fn test_agents_reminders_rendering_visibility() {
        use std::path::Path;

        let mut lines = Vec::new();
        let mut links = Vec::new();
        let results = HashMap::new();
        let expand = HashMap::new();
        let thought_expand = HashMap::new();
        let mut hits = Vec::new();
        let mut thought_hits = Vec::new();
        let mut render = ChatRender {
            lines: &mut lines,
            links: &mut links,
            max_width: 80,
            thinking_concise: false,
            results: &results,
            expand: &expand,
            hits: &mut hits,
            thought_expand: &thought_expand,
            thought_hits: &mut thought_hits,
        };
        let mut state = RenderState::default();

        // 1. 全局与项目 AGENTS.md：不展示
        let global_text = crate::system::format_global_agents_reminder(
            Path::new("/home/user/.vva/AGENTS.md"),
            "global instructions",
        );
        let part_global = ContentPart::text(global_text);
        append_parts(
            &mut render,
            Role::User,
            std::slice::from_ref(&part_global).iter(),
            &mut state,
        );

        let project_text = crate::system::format_project_agents_reminder(
            Path::new("/workspace/AGENTS.md"),
            "project instructions",
        );
        let part_project = ContentPart::text(project_text);
        append_parts(
            &mut render,
            Role::User,
            std::slice::from_ref(&part_project).iter(),
            &mut state,
        );

        assert!(render.lines.is_empty());

        // 2. 按需自动加载的子目录 AGENTS.md：展示 "Load xxx/AGENTS.md"
        let dir_text = crate::system::format_directory_agents_reminder(
            Path::new("/workspace"),
            Path::new("/workspace/src/ui/AGENTS.md"),
            "ui instructions",
        );
        let part_dir = ContentPart::text(dir_text);
        append_parts(
            &mut render,
            Role::User,
            std::slice::from_ref(&part_dir).iter(),
            &mut state,
        );

        let full_output = render
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(full_output.contains("Load src/ui/AGENTS.md"));
        assert!(!full_output.contains("ui instructions"));
    }
}
