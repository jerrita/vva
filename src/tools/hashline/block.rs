//! Conservative block resolution for `PUT N*:` / `CUT N*` / `PUT >N*:`.
//! Brace matching behind a string/comment mask, indent blocks, and Markdown
//! headings; unresolved blocks fail.

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
    let detected = detect_block(lines, index);
    if let Some(end) = detected.filter(|end| *end > index) {
        return finish(start, end, path);
    }
    // 成员行：本行未开启多行块（或仅单行平衡），但可能位于更早行开启的块内部。
    if let Some(opener) = enclosing_opener(lines, index) {
        return Err(format!(
            "not_found: line {start} is inside the block opening at line {} in {path}; use PUT {}*",
            opener + 1,
            opener + 1
        ));
    }
    match detected {
        // 单行平衡括号：finish 恰好产出 single-line node 报错。
        Some(_) => finish(start, index, path),
        None => Err(format!(
            "not_found: no multi-line block opens on line {start} in {path}; use PUT {start}.={start}:"
        )),
    }
}

/// 检测级联：Markdown 标题 → 多行括号块 → 缩进块 → 单行平衡括号。
/// 括号计数与缩进边界都基于字符串/注释掩码，docstring 等跨行字面量
/// 的正文行不会误判为 dedent。
fn detect_block(lines: &[&str], index: usize) -> Option<usize> {
    if let Some(end) = markdown_section(lines, index) {
        return Some(end);
    }
    let masked = mask_code(lines);
    let delimiter_end = delimiter_block(&masked, index);
    // A balanced signature such as `def f():` is not the whole node.
    // Keep multi-line delimiters first so brace blocks retain their closer.
    if let Some(end) = delimiter_end.filter(|end| *end > index) {
        return Some(end);
    }
    indent_block(&masked, index).or(delimiter_end)
}

/// 从最近候选行回扫，返回覆盖 `index` 的块的开启行（0 索引）。
fn enclosing_opener(lines: &[&str], index: usize) -> Option<usize> {
    (0..index).rev().find(|&candidate| {
        !lines[candidate].trim().is_empty()
            && detect_block(lines, candidate).is_some_and(|end| end >= index)
    })
}

fn finish(start: u32, end_index: usize, path: &str) -> Result<(u32, u32), String> {
    let end = end_index as u32 + 1;
    if end <= start {
        return Err(format!(
            "invalid_arguments: line {start} is a single-line node in {path}; use PUT {start}.={start}:"
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
    // 节尾空行不属于节：吞掉会悄悄删除节间分隔。
    while end > index && lines[end].trim().is_empty() {
        end -= 1;
    }
    Some(end)
}
fn delimiter_block(masked: &[String], index: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut seen_open = false;
    for (offset, line) in masked.iter().enumerate().skip(index) {
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
        // A delimiter in a later statement does not open a block here.
        if offset == index && !seen_open {
            return None;
        }
        if seen_open && depth == 0 {
            return Some(offset);
        }
    }
    None
}

/// Blank out string literal and comment content so bracket counting skips
/// delimiters inside them. Heuristic only, no language parser: line comments
/// (`//`; `#` unless followed by `[`, which keeps Rust attributes intact),
/// nestable block comments (`/* */`), double-quoted and backtick strings that
/// may span lines, and single-quoted regions only when they close on the same
/// line so Rust lifetimes and prose apostrophes stay untouched.
fn mask_code(lines: &[&str]) -> Vec<String> {
    let mut masked = Vec::with_capacity(lines.len());
    let mut block_depth = 0usize;
    let mut in_string = false;
    let mut string_delim = b'"';
    for line in lines {
        let bytes = line.as_bytes();
        let mut out = bytes.to_vec();
        let mut i = 0;
        while i < bytes.len() {
            if block_depth > 0 {
                if bytes[i..].starts_with(b"*/") {
                    block_depth -= 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                } else if bytes[i..].starts_with(b"/*") {
                    block_depth += 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                } else {
                    out[i] = b' ';
                    i += 1;
                }
            } else if in_string {
                if bytes[i] == b'\\' {
                    out[i] = b' ';
                    if let Some(slot) = out.get_mut(i + 1) {
                        *slot = b' ';
                    }
                    i += 2;
                } else if bytes[i] == string_delim {
                    in_string = false;
                    out[i] = b' ';
                    i += 1;
                } else {
                    out[i] = b' ';
                    i += 1;
                }
            } else {
                match bytes[i] {
                    b'/' if bytes[i..].starts_with(b"//") || bytes[i..].starts_with(b"/*") => {
                        if bytes[i] == b'/' && bytes[i + 1] == b'/' {
                            out[i..].fill(b' ');
                            break;
                        }
                        block_depth = 1;
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                    }
                    b'#' if bytes.get(i + 1) != Some(&b'[') => {
                        out[i..].fill(b' ');
                        break;
                    }
                    b'"' | b'`' => {
                        in_string = true;
                        string_delim = bytes[i];
                        out[i] = b' ';
                        i += 1;
                    }
                    b'\'' => {
                        // Only a same-line closing quote makes this a string.
                        let mut j = i + 1;
                        let mut close = None;
                        while j < bytes.len() {
                            if bytes[j] == b'\\' {
                                j += 2;
                            } else if bytes[j] == b'\'' {
                                close = Some(j);
                                break;
                            } else {
                                j += 1;
                            }
                        }
                        match close {
                            Some(end) => {
                                out[i..=end].fill(b' ');
                                i = end + 1;
                            }
                            None => i += 1,
                        }
                    }
                    _ => i += 1,
                }
            }
        }
        masked.push(String::from_utf8_lossy(&out).into_owned());
    }
    masked
}

fn indent_block(masked: &[String], index: usize) -> Option<usize> {
    let base = indent(&masked[index]);
    let mut end = None;
    for (offset, line) in masked.iter().enumerate().skip(index + 1) {
        if line.trim().is_empty() {
            continue;
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
