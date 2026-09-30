//! 工具调用头部行：逐工具摘要与展开状态判定。

use super::*;
use super::{pipeline::push_source_row, state::*, tool_block::*};
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

pub(super) fn mutation_counts(
    name: &str,
    args: &serde_json::Value,
) -> Option<(String, usize, usize)> {
    if name != "Edit" {
        return None;
    }
    if let Some(input) = json_str(args, "input") {
        let (added, removed) = patch_counts(input);
        return Some((patch_file_path(input)?.to_string(), added, removed));
    }
    let (added, removed) = edit_counts(args)?;
    Some((json_str(args, "path")?.to_string(), added, removed))
}

pub(super) fn patch_file_path(input: &str) -> Option<&str> {
    for line in input.lines() {
        for prefix in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
            if let Some(path) = line.strip_prefix(prefix) {
                return Some(path.trim());
            }
        }
    }
    None
}

pub(super) fn patch_counts(input: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in input.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("***") || trimmed.starts_with("@@") {
            continue;
        }
        if trimmed.starts_with('+') {
            added += 1;
        } else if trimmed.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
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

pub(super) fn tool_call_detail(name: &str, arguments: &str, result: Option<&ToolResult>) -> String {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    match name {
        "AskUser" => ask_user_detail(&args),
        "web_search" => {
            let query = json_str(&args, "query")
                .map(|query| format!("\"{query}\""))
                .unwrap_or_else(|| arguments.to_string());
            match result.map(web_sources) {
                Some(sources) if !sources.is_empty() => {
                    let count = sources.len();
                    let noun = if count == 1 { "source" } else { "sources" };
                    format!("{query} ({count} {noun})")
                }
                _ => query,
            }
        }
        "web_fetch" => json_str(&args, "url")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        "Read" => {
            let path = json_str(&args, "path")
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| arguments.to_string());
            let offset = args
                .get("offset")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1);
            let limit = args
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .map(|limit| limit.to_string())
                .unwrap_or_else(|| "all".into());
            let byte_offset = args
                .get("byte_offset")
                .and_then(serde_json::Value::as_u64)
                .map(|offset| format!(", byte_offset={offset}"))
                .unwrap_or_default();
            format!("{path} [offset={offset}, limit={limit}{byte_offset}]")
        }
        "Write" => json_str(&args, "path")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        "Edit" => {
            if let Some(input) = json_str(&args, "input") {
                match patch_file_path(input) {
                    Some(path) => {
                        let (added, removed) = patch_counts(input);
                        format!("{path} (+{added}, -{removed})")
                    }
                    None => arguments.to_string(),
                }
            } else {
                match (json_str(&args, "path"), edit_counts(&args)) {
                    (Some(path), Some((added, removed))) => {
                        format!("{path} (+{added}, -{removed})")
                    }
                    (Some(path), None) => path.to_string(),
                    _ => arguments.to_string(),
                }
            }
        }
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
        "Shell" => {
            let intent = json_str(&args, "intent")
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| arguments.to_string());
            // 后台启动标注任务态，与前台执行区分
            if args
                .get("run_in_background")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                format!("{intent} (background)")
            } else {
                intent
            }
        }
        "TaskOutput" | "TaskStop" => json_str(&args, "task_id")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        "Agent" => {
            let agent = json_str(&args, "agent").unwrap_or("agent");
            let description = json_str(&args, "description").unwrap_or("");
            let background = args
                .get("run_in_background")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            let suffix = if background { " (background)" } else { "" };
            match description.is_empty() {
                true => format!("{agent}{suffix}"),
                false => format!("{agent} · {description}{suffix}"),
            }
        }
        "SendMessage" => {
            let session = json_str(&args, "session_id").unwrap_or("");
            match json_str(&args, "text") {
                Some(text) => format!("{session} · \"{}\"", truncate(text, 40)),
                None => session.to_string(),
            }
        }
        "WaitSession" => json_str(&args, "session_id")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
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
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    match name {
        "AskUser" => ask_user_detail(&args),
        "Shell" => json_str(&args, "command")
            .map(|command| format!("$ {command}"))
            .unwrap_or_else(|| arguments.to_string()),
        "Read" | "Write" | "Edit" => json_str(&args, "input")
            .and_then(patch_file_path)
            .or_else(|| json_str(&args, "path"))
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        "Glob" => json_str(&args, "pattern")
            .map(|pattern| format!("\"{pattern}\""))
            .unwrap_or_else(|| arguments.to_string()),
        "Grep" => json_str(&args, "pattern")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        "TaskOutput" | "TaskStop" => json_str(&args, "task_id")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| arguments.to_string()),
        _ => arguments.to_string(),
    }
}

pub(super) fn is_mutation_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write")
}

pub(super) fn edit_uses_patch_body(arguments: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(arguments)
        .is_ok_and(|args| args.get("input").is_some() || args.get("ops").is_some())
}

pub(super) fn default_tool_expand(name: &str) -> Option<ToolExpand> {
    match name {
        "Edit" | "Write" => Some(ToolExpand::Full),
        "Shell" => Some(ToolExpand::Preview),
        // 后台任务工具：预览任务输出/停止结果，点击展开全文
        "TaskOutput" | "TaskStop" => Some(ToolExpand::Preview),
        // 子代理工具：预览最终消息 / 投递回执，点击展开全文
        "Agent" | "SendMessage" | "WaitSession" => Some(ToolExpand::Preview),
        // 外置 web 工具对齐原生 WebSearchCall：仅一行 + 来源列表，不渲染输出块
        _ => None,
    }
}

pub(super) fn push_tool_call(render: &mut ChatRender<'_>, call: &ToolCall) {
    let result = render.results.get(&call.id).copied();
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
    } else if matches!(call.name.as_str(), "web_search" | "web_fetch") {
        bold(Color::Yellow)
    } else {
        bold(Color::Rgb(210, 210, 210))
    };
    let detail_style = fg(Color::Rgb(150, 150, 150));
    let mut spans = vec![
        Span::styled(arrow, fg(Color::Rgb(160, 160, 160))),
        Span::styled(call.name.clone(), name_style),
        Span::raw(" "),
    ];
    let edit_detail = serde_json::from_str::<serde_json::Value>(&call.arguments)
        .ok()
        .and_then(|args| mutation_counts(&call.name, &args));
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
    if let Some(task_id) = result.and_then(|result| result.background_task_id.as_deref())
        && let Some(notice) = render.notices.iter().find(|notice| {
            matches!(&notice.kind, crate::harness::NoticeKind::Task { task_id: id } if id == task_id)
        })
    {
        spans.push(Span::styled(format!(" · {}", notice.text), detail_style));
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
