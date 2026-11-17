//! `[PATH#TAG]` 快照渲染：Read/Grep 输出与编辑结果报告。

use super::session::{HashlineSession, file_tag};
use crate::tools::INLINE_OUTPUT_BUDGET;
use crate::tools::filesystem::{ChangedRange, EditPlan, format_edit_result, line_offsets};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::apply::{Change, LineFile};

pub fn format_read(
    path_label: &str,
    content: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    byte_offset: Option<usize>,
    session: Option<&HashlineSession>,
    canonical: &Path,
) -> Result<String, String> {
    let offset = offset.unwrap_or(1);
    if offset == 0 {
        return Err("offset must be a positive, 1-indexed line number".into());
    }
    if byte_offset.is_some() && limit != Some(1) {
        return Err("byte_offset requires limit=1".into());
    }
    let file = LineFile::parse(content);
    let tag = file_tag(content);
    // 早前部分展示进度：byte_offset 恰好接上时，补全的行可计入 seen。
    let partial = session
        .and_then(|session| session.lookup(canonical, &tag).ok())
        .map(|snap| snap.partial)
        .unwrap_or_default();
    let budget = INLINE_OUTPUT_BUDGET;
    let mut output = format!("[{path_label}#{tag}]\n");
    let mut seen = Vec::new();
    let mut shown = 0;
    let mut index = offset.saturating_sub(1);
    while shown < limit.unwrap_or(usize::MAX) {
        if index >= file.lines.len() {
            if byte_offset.is_some() {
                return Err("byte_offset refers to a line past EOF".into());
            }
            output.push_str("[End of file]");
            record_read(session, canonical, content, seen, Vec::new())?;
            return Ok(output);
        }
        let line = &file.lines[index];
        let skip = byte_offset.unwrap_or(0);
        if skip > line.len() || !line.is_char_boundary(skip) {
            return Err(format!(
                "byte_offset must be a UTF-8 boundary within line {} ({} bytes)",
                index + 1,
                line.len()
            ));
        }
        let prefix = format!("{}:", index + 1);
        let text = &line[skip..];
        let remaining = budget.saturating_sub(output.len());
        if prefix.len() + text.len() + 1 > remaining {
            if shown > 0 {
                output.push_str(&format!(
                    "[Continue with hashline_read offset={}.]",
                    index + 1
                ));
                record_read(session, canonical, content, seen, Vec::new())?;
                return Ok(output);
            }
            let mut cut = remaining.saturating_sub(prefix.len() + 1).min(text.len());
            while !text.is_char_boundary(cut) {
                cut = cut.saturating_sub(1);
            }
            output.push_str(&prefix);
            output.push_str(&text[..cut]);
            output.push_str(&format!(
                "\n[Partial line; continue with hashline_read offset={}, byte_offset={}, limit=1.]",
                index + 1,
                skip + cut
            ));
            record_read(
                session,
                canonical,
                content,
                seen,
                vec![((index + 1) as u32, skip + cut)],
            )?;
            return Ok(output);
        }
        output.push_str(&prefix);
        output.push_str(text);
        output.push('\n');
        let line_no = (index + 1) as u32;
        let completed = skip == 0 || partial.get(&line_no).is_some_and(|bytes| *bytes == skip);
        if completed {
            seen.push(line_no);
        }
        shown += 1;
        index += 1;
    }
    if index < file.lines.len() {
        output.push_str(&format!(
            "[Continue with hashline_read offset={}.]",
            index + 1
        ));
    } else {
        output.push_str("[End of file]");
    }
    record_read(session, canonical, content, seen, Vec::new())?;
    Ok(output)
}

fn record_read(
    session: Option<&HashlineSession>,
    canonical: &Path,
    content: &str,
    seen: Vec<u32>,
    partial: Vec<(u32, usize)>,
) -> Result<(), String> {
    if let Some(session) = session {
        session.record_with_partial(canonical.to_path_buf(), content, seen, partial)?;
    }
    Ok(())
}

pub fn format_grep_file(
    path_label: &str,
    content: &str,
    matches: &[(usize, &str)],
    session: Option<&HashlineSession>,
    canonical: &Path,
) -> Result<String, String> {
    if matches.is_empty() {
        return Ok(String::new());
    }
    let tag = file_tag(content);
    let mut output = format!("[{path_label}#{tag}]\n");
    let mut seen = Vec::new();
    let mut partials: Vec<(u32, usize)> = Vec::new();
    let budget = INLINE_OUTPUT_BUDGET;
    for (index, line) in matches {
        let prefix = format!("{}:", index + 1);
        let remaining = budget.saturating_sub(output.len());
        if prefix.len() + line.len() + 1 > remaining {
            // 超预算的匹配行裁剪展示：不注册 seen，只登记部分进度，
            // 由 Read 的 byte_offset 续读补全后才可编辑。
            let mut cut = remaining.saturating_sub(prefix.len() + 1).min(line.len());
            while cut > 0 && !line.is_char_boundary(cut) {
                cut -= 1;
            }
            output.push_str(&prefix);
            output.push_str(&line[..cut]);
            if cut > 0 {
                output.push_str(&format!(
                    "\n[Partial line; continue with hashline_read offset={}, byte_offset={}, limit=1.]",
                    index + 1,
                    cut
                ));
                partials.push(((*index + 1) as u32, cut));
            } else {
                output.push_str(&format!(
                    "\n[Line too long to display; hashline_read offset={}, limit=1.]",
                    index + 1
                ));
            }
            continue;
        }
        output.push_str(&prefix);
        output.push_str(line);
        output.push('\n');
        seen.push((*index + 1) as u32);
    }
    if let Some(session) = session {
        session.record_with_partial(canonical.to_path_buf(), content, seen, partials)?;
    }
    if output.ends_with('\n') {
        output.pop();
    }
    Ok(output)
}

/// 变更 span 之外的行在新旧快照逐字节相同，已展示状态按行号映射结转：
/// span 之前保持原号，span 之后平移 new-old，span 之内的行随替换丢弃。
fn carried_seen(base_seen: &HashSet<u32>, changes: &[Change]) -> HashSet<u32> {
    let mut carried = HashSet::new();
    'line: for line in base_seen {
        let position = (*line as usize).saturating_sub(1);
        let mut shift: i64 = 0;
        for (old, new) in changes {
            if position < old.start {
                break;
            }
            if position < old.end {
                continue 'line;
            }
            shift += new.len() as i64 - old.len() as i64;
        }
        let mapped = position as i64 + shift;
        if mapped >= 0 {
            carried.insert((mapped + 1) as u32);
        }
    }
    carried
}

pub(super) fn report_update(
    session: &HashlineSession,
    label: impl Into<String>,
    canonical: PathBuf,
    original: &str,
    updated: &str,
    changes: Vec<Change>,
    base_seen: &HashSet<u32>,
) -> Result<String, String> {
    let label = label.into();
    let carried = carried_seen(base_seen, &changes);
    let old_offsets = line_offsets(original);
    let new_offsets = line_offsets(updated);
    let preview = numbered_preview(updated, &changes);
    let plan = EditPlan {
        updated: updated.to_string(),
        changes: changes
            .into_iter()
            .map(|(old, new)| ChangedRange {
                old: old_offsets[old.start]..old_offsets[old.end],
                new: new_offsets[new.start.min(new_offsets.len() - 1)]
                    ..new_offsets[new.end.min(new_offsets.len() - 1)],
            })
            .collect(),
    };
    let diff = format_edit_result(&canonical, original, &plan, false);
    let mut seen: Vec<u32> = carried.iter().copied().collect();
    seen.extend(preview.iter().map(|(n, _)| *n));
    let tag = session.record(canonical, updated, seen)?;
    let mut out = format!("[{label}#{tag}]\n{diff}");
    if !preview.is_empty() {
        out.push_str("\nCurrent snapshot preview (only numbered lines are registered):\n");
        let mut previous = 0;
        for (number, line) in preview {
            if number > previous + 1 {
                out.push_str(&gap_marker(previous + 1, number - 1, &carried));
            }
            out.push_str(&format!("{number}:{line}\n"));
            previous = number;
        }
        if (previous as usize) < new_offsets.len() - 1 {
            out.push_str(&gap_marker(
                previous + 1,
                new_offsets.len() as u32 - 1,
                &carried,
            ));
        }
        out.pop();
    }
    Ok(out)
}

/// 预览间隙提示：早前展示过（行号平移结转）的行无需重新 Read；
/// 从未展示的行才要求先 Read。
fn gap_marker(first: u32, last: u32, carried: &HashSet<u32>) -> String {
    if (first..=last).all(|line| carried.contains(&line)) {
        format!("[Lines {first}-{last} unchanged; shown earlier, editable without hashline_read]\n")
    } else {
        format!("[Lines {first}-{last} not displayed this session; hashline_read before editing]\n")
    }
}

pub(super) fn numbered_preview(text: &str, changes: &[Change]) -> Vec<(u32, String)> {
    let file = LineFile::parse(text);
    let total = file.lines.len();
    let mut shown = std::collections::BTreeSet::new();
    for (_, range) in changes {
        let start = range.start.saturating_sub(3);
        let end = range.end.saturating_add(3).min(total);
        if end - start <= 16 {
            shown.extend(start..end);
        } else {
            shown.extend(start..start + 8);
            shown.extend(end - 8..end);
        }
    }
    file.lines
        .into_iter()
        .enumerate()
        .filter(|(index, _)| shown.contains(index))
        .map(|(index, line)| ((index + 1) as u32, line))
        .collect()
}
