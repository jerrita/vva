//! Conservative block resolution for `PUT N*:` / `CUT N*` / `PUT >N*:`.
//! Brace matching, indent blocks, and Markdown headings; unresolved blocks fail.

pub fn resolve_block(lines: &[&str], start: u32, path: &str) -> Result<(u32, u32), String> {
    if start == 0 {
        return Err("invalid_arguments: block anchor must be a 1-indexed line".into());
    }
    let index = (start as usize).saturating_sub(1);
    if index >= lines.len() {
        return Err(format!(
            "not_found: line {start} does not exist (file has {} lines)",
            lines.len()
        ));
    }
    if lines[index].trim().is_empty() {
        return Err(format!(
            "invalid_arguments: line {start} is blank; block ops need the opener, or use PUT {start}.={start}:"
        ));
    }
    if let Some(span) = markdown_section(lines, index) {
        return finish(start, span, path);
    }
    if let Some(span) = delimiter_block(lines, index) {
        return finish(start, span, path);
    }
    if let Some(span) = indent_block(lines, index) {
        return finish(start, span, path);
    }
    Err(format!(
        "not_found: no multi-line block opens on line {start} in {path}; use PUT {start}.={start}: / CUT {start}.={start} / PUT >{start}:"
    ))
}

fn finish(start: u32, end_index: usize, path: &str) -> Result<(u32, u32), String> {
    let end = end_index as u32 + 1;
    if end <= start {
        return Err(format!(
            "invalid_arguments: line {start} is a single-line node in {path}; use PUT {start}.={start}: or CUT {start}.={start}"
        ));
    }
    Ok((start, end))
}

fn heading_level(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    if bytes.first().copied() != Some(b'#') {
        return None;
    }
    let mut level = 0;
    while level < bytes.len() && bytes[level] == b'#' && level < 6 {
        level += 1;
    }
    (level > 0
        && bytes
            .get(level)
            .is_some_and(|byte| *byte == b' ' || *byte == b'\t'))
    .then_some(level)
}

fn markdown_section(lines: &[&str], index: usize) -> Option<usize> {
    let level = heading_level(lines[index])?;
    let mut end = index;
    for (offset, line) in lines.iter().enumerate().skip(index + 1) {
        if heading_level(line).is_some_and(|next| next <= level) {
            break;
        }
        end = offset;
    }
    Some(end)
}

fn delimiter_block(lines: &[&str], index: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut seen_open = false;
    for (offset, line) in lines.iter().enumerate().skip(index) {
        for byte in line.bytes() {
            match byte {
                b'{' | b'[' | b'(' => {
                    depth += 1;
                    seen_open = true;
                }
                b'}' | b']' | b')' if depth > 0 => depth -= 1,
                _ => {}
            }
        }
        if seen_open && depth == 0 {
            return Some(offset);
        }
    }
    None
}

fn indent_block(lines: &[&str], index: usize) -> Option<usize> {
    let base = indent(lines[index]);
    let mut end = None;
    for (offset, line) in lines.iter().enumerate().skip(index + 1) {
        if line.trim().is_empty() {
            if end.is_some() {
                continue;
            }
            break;
        }
        if indent(line) <= base {
            break;
        }
        end = Some(offset);
    }
    end
}

fn indent(line: &str) -> usize {
    line.bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}
