//! 工具调用头部行：逐工具摘要与展开状态判定。

use super::*;
use super::{state::*, tool_block::*};

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

pub(super) fn tool_call_detail(name: &str, arguments: &str, result: Option<&ToolResult>) -> String {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    match name {
        "AskUser" => ask_user_detail(&args),
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
        "Shell" => json_str(&args, "intent")
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
    let arrow = if has_content_block {
        if expanded { "↓ " } else { "→ " }
    } else {
        "* "
    };
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
