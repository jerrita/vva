//! 工具调用头部行：逐工具摘要与展开状态判定。

use super::*;
use super::{patch::*, pipeline::push_source_row, state::*, tool_block::*};
use crate::tools::names;
use crate::ui::text::truncate;
use crate::ui::views::{bold, fg};

pub(super) fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

pub(super) fn edit_counts(args: &serde_json::Value) -> Option<(usize, usize)> {
    if let Some(edits) = args.get("edits") {
        edits
            .as_array()?
            .iter()
            .try_fold((0, 0), |(added, removed), edit| {
                Some((
                    added + line_count(json_str(edit, "new_string")?),
                    removed + line_count(json_str(edit, "old_string")?),
                ))
            })
    } else {
        Some((
            line_count(json_str(args, "new_string")?),
            line_count(json_str(args, "old_string")?),
        ))
    }
}

pub(super) fn line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

pub(super) fn glob_match_count(result: &ToolResult) -> Option<usize> {
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

/// web_search 工具输出中的编号条目 → (title, url)。
/// 与工具输出格式耦合：`N. title` 行后紧跟 URL 行（title 缺省时该行为 `N. url`）。
pub(super) fn web_sources(result: &ToolResult) -> Vec<(String, String)> {
    if result.is_error {
        return Vec::new();
    }
    let lines: Vec<&str> = result.content.lines().collect();
    let mut sources = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let entry = numbered_entry(lines[index]);
        if let Some(next) = lines.get(index + 1)
            && let Some(entry) = entry
            && next.trim().starts_with("http")
        {
            sources.push((entry.trim().to_string(), next.trim().to_string()));
            index += 2;
            continue;
        }
        if let Some(entry) = entry
            && entry.trim().starts_with("http")
        {
            sources.push((entry.trim().to_string(), entry.trim().to_string()));
        }
        index += 1;
    }
    sources
}

fn numbered_entry(line: &str) -> Option<&str> {
    let (index, entry) = line.split_once(". ")?;
    (!index.is_empty() && index.len() <= 3 && index.chars().all(|ch| ch.is_ascii_digit()))
        .then_some(entry)
}

/// 字符串参数，缺省时回退为原始参数文本
fn str_or_raw(args: &serde_json::Value, key: &str, raw: &str) -> String {
    json_str(args, key).map_or_else(|| raw.to_string(), ToOwned::to_owned)
}

/// 带双引号的字符串参数，缺省时回退为原始参数文本
fn quoted_or_raw(args: &serde_json::Value, key: &str, raw: &str) -> String {
    json_str(args, key).map_or_else(|| raw.to_string(), |value| format!("\"{value}\""))
}

fn is_background(args: &serde_json::Value) -> bool {
    args.get("run_in_background")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

/// `label (N noun)`，按数量选择单复数
fn with_count(label: String, count: usize, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{label} ({count} {noun})")
}

/// 头部显示标签：按具体工具名映射编辑协议动词，其余沿用工具名
pub(super) fn tool_label(name: &str) -> &str {
    match name {
        names::READ | names::HASHLINE_READ => "Read",
        names::GREP | names::HASHLINE_GREP => "Grep",
        names::EDIT | names::HASHLINE_EDIT => "Edit",
        names::APPLY_PATCH => "Patch",
        names::WRITE => "Write",
        _ => name,
    }
}

/// freeform 工具的 `arguments` 是补丁原文而非 JSON
pub(super) fn is_freeform_edit(name: &str) -> bool {
    matches!(name, names::APPLY_PATCH | names::HASHLINE_EDIT)
}

/// 变更类工具的头部摘要：目标 + 计数尾注
pub(super) fn change_summary(name: &str, arguments: &str, complete: bool) -> Option<Summary> {
    match name {
        names::APPLY_PATCH => summarize(&parse_apply_patch(arguments, complete)),
        names::HASHLINE_EDIT => summarize(&parse_hashline_input(arguments, complete)),
        names::EDIT => {
            let args = serde_json::from_str::<serde_json::Value>(arguments).ok()?;
            let target = json_str(&args, "path")?.to_string();
            let tail = edit_counts(&args)
                .map_or(Tail::None, |(added, removed)| Tail::Counts(added, removed));
            Some(Summary { target, tail })
        }
        _ => None,
    }
}

pub(super) fn tool_call_detail(name: &str, arguments: &str, result: Option<&ToolResult>) -> String {
    if is_freeform_edit(name) {
        return change_summary(name, arguments, result.is_some())
            .map(|summary| summary.text())
            .unwrap_or_default();
    }
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    match name {
        "AskUser" => ask_user_detail(&args),
        "web_search" => {
            let query = quoted_or_raw(&args, "query", arguments);
            match result.map(web_sources) {
                Some(sources) if !sources.is_empty() => {
                    with_count(query, sources.len(), "source", "sources")
                }
                _ => query,
            }
        }
        "web_fetch" => str_or_raw(&args, "url", arguments),
        names::READ | names::HASHLINE_READ => {
            let path = str_or_raw(&args, "path", arguments);
            let number = |key: &str| args.get(key).and_then(serde_json::Value::as_u64);
            let offset = number("offset").unwrap_or(1);
            let limit = number("limit").map_or_else(|| "all".into(), |limit| limit.to_string());
            let byte_offset = number("byte_offset")
                .map(|offset| format!(", byte_offset={offset}"))
                .unwrap_or_default();
            format!("{path} [offset={offset}, limit={limit}{byte_offset}]")
        }
        names::WRITE => str_or_raw(&args, "path", arguments),
        names::EDIT => change_summary(name, arguments, true)
            .map_or_else(|| arguments.to_string(), |summary| summary.text()),
        names::GLOB => {
            let pattern = quoted_or_raw(&args, "pattern", arguments);
            match result.and_then(glob_match_count) {
                Some(count) => with_count(pattern, count, "match", "matches"),
                None => pattern,
            }
        }
        names::GREP | names::HASHLINE_GREP => {
            let Some(pattern) = json_str(&args, "pattern") else {
                return arguments.to_string();
            };
            match json_str(&args, "include") {
                Some(include) if !include.is_empty() => format!("{include}, {pattern}"),
                _ => pattern.to_string(),
            }
        }
        names::SHELL => {
            let intent = str_or_raw(&args, "intent", arguments);
            // 后台启动标注任务态，与前台执行区分
            if is_background(&args) {
                format!("{intent} (background)")
            } else {
                intent
            }
        }
        "TaskOutput" | "TaskStop" => str_or_raw(&args, "task_id", arguments),
        "Agent" => {
            let agent = json_str(&args, "agent").unwrap_or("agent");
            let description = json_str(&args, "description").unwrap_or("");
            let suffix = if is_background(&args) {
                " (background)"
            } else {
                ""
            };
            if description.is_empty() {
                format!("{agent}{suffix}")
            } else {
                format!("{agent} · {description}{suffix}")
            }
        }
        "SendMessage" => {
            let session = json_str(&args, "session_id").unwrap_or("");
            match json_str(&args, "text") {
                Some(text) => format!("{session} · \"{}\"", truncate(text, 40)),
                None => session.to_string(),
            }
        }
        _ => arguments.to_string(),
    }
}

pub(super) fn ask_user_detail(args: &serde_json::Value) -> String {
    let Some(questions) = args.get("questions").and_then(serde_json::Value::as_array) else {
        return "user input".into();
    };
    if questions.len() != 1 {
        return format!("{} questions", questions.len());
    }
    let question = &questions[0];
    let header = json_str(question, "header").filter(|header| !header.is_empty());
    let mode = if question
        .get("multi_select")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        "多选"
    } else {
        "单选"
    };
    match header {
        Some(header) => format!("{header} · {mode}"),
        None => mode.to_string(),
    }
}

pub(super) fn tool_block_command(name: &str, arguments: &str) -> String {
    // freeform 工具的 arguments 是补丁原文，不经 JSON 解析
    if is_freeform_edit(name) {
        return arguments.to_string();
    }
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    match name {
        "AskUser" => ask_user_detail(&args),
        names::SHELL => json_str(&args, "command")
            .map_or_else(|| arguments.to_string(), |command| format!("$ {command}")),
        names::READ | names::HASHLINE_READ | names::WRITE | names::EDIT => {
            str_or_raw(&args, "path", arguments)
        }
        names::GLOB => quoted_or_raw(&args, "pattern", arguments),
        names::GREP | names::HASHLINE_GREP => str_or_raw(&args, "pattern", arguments),
        "TaskOutput" | "TaskStop" => str_or_raw(&args, "task_id", arguments),
        _ => arguments.to_string(),
    }
}

pub(super) fn is_mutation_tool(name: &str) -> bool {
    matches!(
        name,
        names::EDIT | names::WRITE | names::APPLY_PATCH | names::HASHLINE_EDIT
    )
}

pub(super) fn default_tool_expand(name: &str) -> Option<ToolExpand> {
    match name {
        names::EDIT | names::WRITE | names::APPLY_PATCH | names::HASHLINE_EDIT => {
            Some(ToolExpand::Full)
        }
        names::SHELL => Some(ToolExpand::Preview),
        // 后台任务工具：预览任务输出/停止结果，点击展开全文
        "TaskOutput" | "TaskStop" => Some(ToolExpand::Preview),
        // 子代理工具：预览最终消息 / 投递回执，点击展开全文
        "Agent" | "SendMessage" => Some(ToolExpand::Preview),
        // 外置 web 工具对齐原生 WebSearchCall：仅一行 + 来源列表，不渲染输出块
        _ => None,
    }
}

pub(super) fn push_tool_call(render: &mut ChatRender<'_>, call: &ToolCall) {
    let result = render.results.get(&call.id).copied();
    let task = result
        .and_then(|result| result.background_task_id.as_ref())
        .and_then(|id| render.tasks.get(id));
    let display_result = task.zip(result).map(|(task, result)| {
        let mut result = result.clone();
        result.content.clone_from(&task.output);
        result.is_error = task.status == crate::model::BackgroundTaskStatus::Failed;
        result.task = Some(task.clone());
        let mut errors: Vec<_> = render
            .query_owners
            .iter()
            .filter(|(_, owner)| *owner == &call.id)
            .filter_map(|(id, _)| {
                render
                    .results
                    .get(id)
                    .filter(|result| result.is_error)
                    .map(|result| (id, result))
            })
            .collect();
        errors.sort_by_key(|(id, _)| *id);
        for (_, error) in errors {
            result
                .content
                .push_str(&format!("\n查询失败: {}", error.content));
        }
        result
    });
    let result = display_result.as_ref().or(result);
    let expand = render
        .expand
        .get(&call.id)
        .copied()
        .or_else(|| default_tool_expand(&call.name));
    let has_content_block = expand.is_some();
    let expanded = !matches!(expand, Some(ToolExpand::Hidden) | None);
    let detail = tool_call_detail(&call.name, &call.arguments, result);
    let is_web_tool = matches!(call.name.as_str(), "web_search" | "web_fetch");
    let arrow = if is_web_tool {
        // 与原生 WebSearchCall 行一致：无箭头前缀
        ""
    } else if has_content_block {
        if expanded { "↓ " } else { "→ " }
    } else {
        "* "
    };
    let is_error = result.is_some_and(|result| result.is_error);
    // 外置 web 工具与原生 server tool 同名，头部样式对齐原生（黄色加粗 label）
    let name_style = if is_error {
        bold(Color::Rgb(220, 100, 100))
    } else if is_web_tool {
        bold(Color::Yellow)
    } else {
        bold(Color::Rgb(210, 210, 210))
    };
    let detail_style = fg(Color::Rgb(150, 150, 150));
    let mut spans = vec![
        Span::styled(arrow, fg(Color::Rgb(160, 160, 160))),
        Span::styled(tool_label(&call.name).to_string(), name_style),
        Span::raw(" "),
    ];
    match change_summary(&call.name, &call.arguments, result.is_some()) {
        Some(summary) => spans.extend(summary.spans(detail_style)),
        None => spans.push(Span::styled(detail, detail_style)),
    }
    if let Some(task) = task {
        spans.push(Span::styled(
            format!(" · {}", task.label()),
            if task.status == crate::model::BackgroundTaskStatus::Running {
                fg(Color::Rgb(220, 140, 170))
            } else {
                detail_style
            },
        ));
    }
    let header_start = render.lines.len();
    let wrapped = wrap_spans_to_lines(&spans, render.max_width, "", "   ");
    render.lines.extend(wrapped);
    let header_end = render.lines.len();
    // 外置 web 搜索与原生 WebSearchCall 同构：行下直接展示来源列表（title 居左、域名居右）
    if call.name == "web_search" && !is_error {
        for (title, url) in result.map(web_sources).unwrap_or_default() {
            push_source_row(render.lines, &url, &title, render.max_width);
        }
    }
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
    if has_content_block {
        render.hits.push(ToolHit {
            id: call.id.clone(),
            header_start,
            header_end,
            block_start,
            block_end,
            truncated,
            full_only: is_mutation_tool(&call.name),
        });
    }
}
