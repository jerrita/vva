//! 文件系统工具集：Read / Write / Edit / Glob / Grep 与共享的路径、遍历、diff 基建。

mod edit;
mod glob;
mod grep;
mod read;
#[cfg(test)]
mod tests;
mod write;

pub use edit::EditTool;
pub(crate) use edit::{
    ChangedRange, EditPlan, commit_text_edit, edit_lock, format_edit_result, persist_edit,
};
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

use ::glob::{MatchOptions, Pattern};
use std::fs;
use std::path::{Path, PathBuf};

use super::Tool;

const MAX_RESULTS: usize = 100;
const MAX_BRACE_ALTS: usize = 256;
const GLOB_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// 工具入参统一解析：JSON 失败时携带工具名报错。
pub(super) fn parse_args<T: serde::de::DeserializeOwned>(
    arguments: &str,
    tool: &str,
) -> Result<T, String> {
    serde_json::from_str(arguments)
        .map_err(|error| format!("Invalid arguments for {tool}: {error}"))
}

fn strip_wrapping_quotes(mut value: &str) -> &str {
    loop {
        let bytes = value.as_bytes();
        if bytes.len() < 2 {
            return value;
        }
        let quote = bytes[0];
        if (quote == b'"' || quote == b'\'') && bytes[bytes.len() - 1] == quote {
            value = &value[1..value.len() - 1];
            continue;
        }
        return value;
    }
}

pub fn resolve_path(working_dir: &Path, path: &str) -> PathBuf {
    let path = strip_wrapping_quotes(path);
    if path.is_empty() || path == "." {
        return working_dir.to_path_buf();
    }
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    }
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Failed to access {}: {error}", path.display()))?;
    if metadata.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    if metadata.is_dir() {
        visit_files(path, files)?;
    }
    Ok(())
}

fn visit_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(path)
        .map_err(|error| format!("Failed to read directory {}: {error}", path.display()))?
    {
        let entry = entry.map_err(|error| format!("Failed to read directory entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            visit_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn sort_by_mtime(files: &mut [PathBuf]) -> Result<(), String> {
    let mut modified = std::collections::HashMap::new();
    for path in files.iter() {
        let time = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                format!(
                    "Failed to read modification time {}: {error}",
                    path.display()
                )
            })?;
        modified.insert(path.clone(), time);
    }
    files.sort_by(|a, b| modified[b].cmp(&modified[a]).then_with(|| a.cmp(b)));
    Ok(())
}

fn relative_to<'a>(root: &'a Path, path: &'a Path, root_is_file: bool) -> &'a Path {
    if root_is_file {
        path.file_name().map(Path::new).unwrap_or(path)
    } else {
        path.strip_prefix(root).unwrap_or(path)
    }
}

fn expand_braces(pattern: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    expand_braces_into(pattern, &mut out)?;
    Ok(out)
}

fn expand_braces_into(pattern: &str, out: &mut Vec<String>) -> Result<(), String> {
    if out.len() >= MAX_BRACE_ALTS {
        return Err(format!(
            "brace expansion exceeded {MAX_BRACE_ALTS} alternatives"
        ));
    }
    match next_expandable_group(pattern) {
        None => {
            out.push(pattern.to_string());
            Ok(())
        }
        Some((start, end, alts)) => {
            let prefix = &pattern[..start];
            let suffix = &pattern[end..];
            for alt in alts {
                expand_braces_into(&format!("{prefix}{alt}{suffix}"), out)?;
            }
            Ok(())
        }
    }
}

fn next_expandable_group(pattern: &str) -> Option<(usize, usize, Vec<String>)> {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'{'
            && let Some((end, alts)) = parse_brace_alts(pattern, i)
        {
            return Some((i, end, alts));
        }
        i += 1;
    }
    None
}

fn parse_brace_alts(pattern: &str, start: usize) -> Option<(usize, Vec<String>)> {
    let bytes = pattern.as_bytes();
    let mut depth = 0;
    let mut alt_start = start + 1;
    let mut alts = Vec::new();
    let mut has_comma = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 1;
                if i < bytes.len() {
                    i += 1;
                }
                continue;
            }
            b'{' => depth += 1,
            b',' if depth == 1 => {
                has_comma = true;
                alts.push(pattern[alt_start..i].to_string());
                alt_start = i + 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if !has_comma {
                        return None;
                    }
                    alts.push(pattern[alt_start..i].to_string());
                    return Some((i + 1, alts));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_glob_patterns(pattern: &str) -> Result<Vec<Pattern>, String> {
    expand_braces(strip_wrapping_quotes(pattern))?
        .into_iter()
        .map(|item| Pattern::new(&item).map_err(|error| error.to_string()))
        .collect()
}

fn matches_any(patterns: &[Pattern], path: &Path) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern.matches_path_with(path, GLOB_OPTIONS))
}

fn unified_diff(
    path: &Path,
    original: &str,
    updated: &str,
    old_offset: usize,
    new_offset: usize,
) -> String {
    let original_lines: Vec<_> = original.split_inclusive('\n').collect();
    let updated_lines: Vec<_> = updated.split_inclusive('\n').collect();

    let prefix = original_lines
        .iter()
        .zip(&updated_lines)
        .take_while(|(original, updated)| original == updated)
        .count();
    let suffix = original_lines[prefix..]
        .iter()
        .rev()
        .zip(updated_lines[prefix..].iter().rev())
        .take_while(|(original, updated)| original == updated)
        .count();
    let original_changed_end = original_lines.len() - suffix;
    let updated_changed_end = updated_lines.len() - suffix;
    let context_start = prefix.saturating_sub(3);
    let original_end = (original_changed_end + 3).min(original_lines.len());
    let updated_end = (updated_changed_end + 3).min(updated_lines.len());
    let original_count = original_end - context_start;
    let updated_count = updated_end - context_start;

    let format_range = |start: usize, count: usize| match count {
        0 => format!("{start},0"),
        1 => (start + 1).to_string(),
        _ => format!("{},{}", start + 1, count),
    };

    let mut diff = format!(
        "--- {}\n+++ {}\n@@ -{} +{} @@",
        path.display(),
        path.display(),
        format_range(old_offset + context_start, original_count),
        format_range(new_offset + context_start, updated_count),
    );
    let mut push_line = |marker: char, line: &str| {
        diff.push_str(&format!(
            "\n{marker}{}",
            line.strip_suffix('\n').unwrap_or(line)
        ));
        if !line.ends_with('\n') {
            diff.push_str("\n\\ No newline at end of file");
        }
    };
    for line in &original_lines[context_start..prefix] {
        push_line(' ', line);
    }
    for line in &original_lines[prefix..original_changed_end] {
        push_line('-', line);
    }
    for line in &updated_lines[prefix..updated_changed_end] {
        push_line('+', line);
    }
    for line in &original_lines[original_changed_end..original_end] {
        push_line(' ', line);
    }
    diff
}
