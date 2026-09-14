//! Canonical hashline patch grammar. Unknown rows and non-canonical range
//! spellings are rejected; omp's parser recovery is not a second syntax.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub path: String,
    pub tag: String,
    pub ops: Vec<Op>,
    pub file_op: Option<FileOp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Rem,
    Move { dest: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Replace {
        start: u32,
        end: u32,
        body: Vec<String>,
    },
    ReplaceBlock {
        line: u32,
        body: Vec<String>,
    },
    InsertBefore {
        line: u32,
        body: Vec<String>,
    },
    InsertAfter {
        line: u32,
        body: Vec<String>,
    },
    InsertAfterBlock {
        line: u32,
        body: Vec<String>,
    },
    Append {
        body: Vec<String>,
    },
    InsertBof {
        body: Vec<String>,
    },
    Cut {
        start: u32,
        end: u32,
        register: Option<String>,
    },
    CutBlock {
        line: u32,
        register: Option<String>,
    },
    PasteBefore {
        line: u32,
        register: Option<String>,
    },
    PasteAfter {
        line: u32,
        register: Option<String>,
    },
    PasteEof {
        register: Option<String>,
    },
    PasteBof {
        register: Option<String>,
    },
    PasteRange {
        start: u32,
        end: u32,
        register: String,
    },
    PasteBlock {
        line: u32,
        register: String,
    },
    PasteAfterBlock {
        line: u32,
        register: Option<String>,
    },
}

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const MAX_RANGE: u32 = 100_000;

pub fn parse_patch(input: &str) -> Result<Vec<Section>, String> {
    let mut rows: Vec<(u32, String)> = input
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            (
                index as u32 + 1,
                line.strip_suffix('\r').unwrap_or(line).to_string(),
            )
        })
        .collect();
    if rows.last().is_some_and(|(_, line)| line.is_empty()) {
        rows.pop();
    }
    let mut start = 0;
    let mut end = rows.len();
    while start < end && rows[start].1.trim().is_empty() {
        start += 1;
    }
    while end > start && rows[end - 1].1.trim().is_empty() {
        end -= 1;
    }
    let rows = &rows[start..end];
    if rows.is_empty() {
        return Err("invalid_arguments: no hashline sections found in input".into());
    }
    let rows = if rows[0].1.trim_end() == BEGIN {
        if rows.last().map(|row| row.1.trim_end()) != Some(END) {
            return Err(
                "invalid_arguments: patch starts with *** Begin Patch but has no *** End Patch"
                    .into(),
            );
        }
        let inner = &rows[1..rows.len() - 1];
        if inner.is_empty() {
            return Err("invalid_arguments: no hashline sections found in input".into());
        }
        inner
    } else if rows
        .iter()
        .any(|row| row.1.trim_end() == BEGIN || row.1.trim_end() == END)
    {
        return Err(
            "invalid_arguments: *** Begin Patch / *** End Patch must wrap the whole payload".into(),
        );
    } else {
        rows
    };

    let mut sections = Vec::new();
    let mut current: Option<OpenSection> = None;
    for (line_num, line) in rows {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if let Some(section) = current.as_mut() {
                section.flush_body(*line_num)?;
            }
            continue;
        }
        if let Some((path, tag)) = parse_header(trimmed) {
            if let Some(section) = current.take() {
                sections.push(section.finish(*line_num)?);
            }
            current = Some(OpenSection {
                path,
                tag,
                ops: Vec::new(),
                file_op: None,
                body: None,
            });
            continue;
        }
        let Some(section) = current.as_mut() else {
            return Err(format!(
                "invalid_arguments: line {line_num}: payload has no [PATH#TAG] header"
            ));
        };
        if let Some(header) = parse_hunk(trimmed, *line_num)? {
            section.flush_body(*line_num)?;
            section.push_hunk(header, *line_num)?;
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            let Some(body) = section.body.as_mut() else {
                return Err(format!(
                    "invalid_arguments: line {line_num}: `+` body row has no PUT header"
                ));
            };
            body.push(text.to_string());
            continue;
        }
        return Err(format!(
            "invalid_arguments: line {line_num}: expected [PATH#TAG], PUT/CUT/REM/MV, or a `+` body row; got {trimmed:?}"
        ));
    }
    if let Some(section) = current.take() {
        sections.push(section.finish(rows.last().map(|row| row.0).unwrap_or(1))?);
    }
    if sections.is_empty() {
        return Err("invalid_arguments: no hashline sections found in input".into());
    }
    Ok(sections)
}

struct OpenSection {
    path: String,
    tag: String,
    ops: Vec<Op>,
    file_op: Option<FileOp>,
    body: Option<Vec<String>>,
}

impl OpenSection {
    fn flush_body(&mut self, line_num: u32) -> Result<(), String> {
        let Some(body) = self.body.take() else {
            return Ok(());
        };
        if body.is_empty() {
            return Err(format!(
                "invalid_arguments: line {line_num}: PUT with `:` requires one or more `+` body rows"
            ));
        }
        let Some(
            Op::Replace { body: slot, .. }
            | Op::ReplaceBlock { body: slot, .. }
            | Op::InsertBefore { body: slot, .. }
            | Op::InsertAfter { body: slot, .. }
            | Op::InsertAfterBlock { body: slot, .. }
            | Op::Append { body: slot }
            | Op::InsertBof { body: slot },
        ) = self.ops.last_mut()
        else {
            return Err(format!(
                "invalid_arguments: line {line_num}: unexpected body rows"
            ));
        };
        *slot = body;
        Ok(())
    }

    fn push_hunk(&mut self, header: Hunk, line_num: u32) -> Result<(), String> {
        match header {
            Hunk::File(op) => {
                if self.file_op.is_some() {
                    return Err(format!(
                        "invalid_arguments: line {line_num}: multiple REM/MV in one section"
                    ));
                }
                self.file_op = Some(op);
            }
            Hunk::Op { op, wants_body } => {
                self.ops.push(op);
                if wants_body {
                    self.body = Some(Vec::new());
                }
            }
        }
        Ok(())
    }

    fn finish(mut self, line_num: u32) -> Result<Section, String> {
        self.flush_body(line_num)?;
        if self.ops.is_empty() && self.file_op.is_none() {
            return Err(format!(
                "invalid_arguments: [{}#{}] has no operations",
                self.path, self.tag
            ));
        }
        Ok(Section {
            path: self.path,
            tag: self.tag,
            ops: self.ops,
            file_op: self.file_op,
        })
    }
}

enum Hunk {
    Op { op: Op, wants_body: bool },
    File(FileOp),
}

fn parse_header(line: &str) -> Option<(String, String)> {
    let body = line.strip_prefix('[')?.strip_suffix(']')?;
    let (path, tag) = body.rsplit_once('#')?;
    if path.is_empty()
        || path.contains('#')
        || tag.len() != 4
        || !tag.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some((path.to_string(), tag.to_ascii_uppercase()))
}

fn parse_hunk(line: &str, line_num: u32) -> Result<Option<Hunk>, String> {
    let line = line.trim();
    if let Some(rest) = keyword_tail(line, "REM") {
        if !rest.trim().is_empty() {
            return Err(format!(
                "invalid_arguments: line {line_num}: REM takes no arguments"
            ));
        }
        return Ok(Some(Hunk::File(FileOp::Rem)));
    }
    if let Some(rest) = keyword_tail(line, "MV") {
        let dest = unquote_path(rest.trim()).ok_or_else(|| {
            format!("invalid_arguments: line {line_num}: MV destination is invalid")
        })?;
        if dest.is_empty() {
            return Err(format!(
                "invalid_arguments: line {line_num}: MV requires a destination path"
            ));
        }
        return Ok(Some(Hunk::File(FileOp::Move { dest })));
    }
    if let Some(rest) = keyword_tail(line, "PUT") {
        return Ok(Some(parse_put(rest.trim_start(), line_num)?));
    }
    if let Some(rest) = keyword_tail(line, "CUT") {
        return Ok(Some(parse_cut(rest.trim_start(), line_num)?));
    }
    Ok(None)
}

fn keyword_tail<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    if rest.is_empty() || rest.starts_with(':') || rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

fn parse_put(rest: &str, line_num: u32) -> Result<Hunk, String> {
    let (locator, register, colon) = split_locator(rest, line_num)?;
    if colon && register.is_some() {
        return Err(format!(
            "invalid_arguments: line {line_num}: register PUT takes no `:` body; use `PUT {locator} @{reg}`",
            reg = register.as_deref().unwrap_or_default()
        ));
    }
    let wants_body = colon;
    let op = match (locator, register, colon) {
        (Locator::Range { start, end }, None, true) => Op::Replace {
            start,
            end,
            body: Vec::new(),
        },
        (Locator::Range { start, end }, Some(register), false) => Op::PasteRange {
            start,
            end,
            register,
        },
        (Locator::Block { line }, None, true) => Op::ReplaceBlock {
            line,
            body: Vec::new(),
        },
        (Locator::Block { line }, Some(register), false) => Op::PasteBlock { line, register },
        (Locator::Before { line: 1 }, None, true) => Op::InsertBof { body: Vec::new() },
        (Locator::Before { line: 1 }, register, false) => Op::PasteBof { register },
        (Locator::Before { line }, None, true) => Op::InsertBefore {
            line,
            body: Vec::new(),
        },
        (Locator::Before { line }, register, false) => Op::PasteBefore { line, register },
        (Locator::After { line }, None, true) => Op::InsertAfter {
            line,
            body: Vec::new(),
        },
        (Locator::After { line }, register, false) => Op::PasteAfter { line, register },
        (Locator::AfterBlock { line }, None, true) => Op::InsertAfterBlock {
            line,
            body: Vec::new(),
        },
        (Locator::AfterBlock { line }, register, false) => Op::PasteAfterBlock { line, register },
        (Locator::Eof, None, true) => Op::Append { body: Vec::new() },
        (Locator::Eof, register, false) => Op::PasteEof { register },
        (Locator::Range { .. } | Locator::Block { .. }, None, false) => {
            return Err(format!(
                "invalid_arguments: line {line_num}: range/block paste requires a named @register"
            ));
        }
        (locator, Some(_), true) => {
            return Err(format!(
                "invalid_arguments: line {line_num}: {locator} register paste takes no body"
            ));
        }
    };
    if matches!(op, Op::Replace { start, end, .. } if end < start) {
        return Err(format!(
            "invalid_arguments: line {line_num}: replace_range end is before start"
        ));
    }
    Ok(Hunk::Op { op, wants_body })
}

fn parse_cut(rest: &str, line_num: u32) -> Result<Hunk, String> {
    let (locator, register, colon) = split_locator(rest, line_num)?;
    if colon {
        return Err(format!(
            "invalid_arguments: line {line_num}: CUT takes no body rows"
        ));
    }
    let op = match locator {
        Locator::Range { start, end } => {
            if end < start {
                return Err(format!(
                    "invalid_arguments: line {line_num}: CUT end is before start"
                ));
            }
            Op::Cut {
                start,
                end,
                register,
            }
        }
        Locator::Block { line } => Op::CutBlock { line, register },
        other => {
            return Err(format!(
                "invalid_arguments: line {line_num}: CUT requires N.=M or N*; got {other}"
            ));
        }
    };
    Ok(Hunk::Op {
        op,
        wants_body: false,
    })
}

#[derive(Debug)]
enum Locator {
    Range { start: u32, end: u32 },
    Block { line: u32 },
    Before { line: u32 },
    After { line: u32 },
    AfterBlock { line: u32 },
    Eof,
}

impl std::fmt::Display for Locator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Range { start, end } => write!(f, "{start}.={end}"),
            Self::Block { line } => write!(f, "{line}*"),
            Self::Before { line } => write!(f, "<{line}"),
            Self::After { line } => write!(f, ">{line}"),
            Self::AfterBlock { line } => write!(f, ">{line}*"),
            Self::Eof => write!(f, ">$"),
        }
    }
}

fn split_locator(rest: &str, line_num: u32) -> Result<(Locator, Option<String>, bool), String> {
    let rest = rest.trim();
    let (locator, after) = parse_locator(rest, line_num)?;
    let after = after.trim_start();
    let (register, after) = if let Some(name) = after.strip_prefix('@') {
        let len = name
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            .count();
        if len == 0 {
            return Err(format!(
                "invalid_arguments: line {line_num}: empty register name"
            ));
        }
        (Some(name[..len].to_string()), name[len..].trim_start())
    } else {
        (None, after)
    };
    let colon = after.starts_with(':');
    let after = if colon { after[1..].trim() } else { after };
    if !after.is_empty() {
        return Err(format!(
            "invalid_arguments: line {line_num}: trailing junk after locator: {after:?}"
        ));
    }
    Ok((locator, register, colon))
}

fn parse_locator(rest: &str, line_num: u32) -> Result<(Locator, &str), String> {
    if let Some(after) = rest.strip_prefix('>') {
        let after = after.trim_start();
        if let Some(tail) = after.strip_prefix('$') {
            return Ok((Locator::Eof, tail));
        }
        let (line, used) = parse_lid(after, line_num)?;
        let tail = &after[used..];
        if let Some(tail) = tail.strip_prefix('*') {
            return Ok((Locator::AfterBlock { line }, tail));
        }
        return Ok((Locator::After { line }, tail));
    }
    if let Some(after) = rest.strip_prefix('<') {
        let after = after.trim_start();
        let (line, used) = parse_lid(after, line_num)?;
        return Ok((Locator::Before { line }, &after[used..]));
    }
    let (start, used) = parse_lid(rest, line_num)?;
    let tail = &rest[used..];
    if let Some(tail) = tail.strip_prefix("*") {
        return Ok((Locator::Block { line: start }, tail));
    }
    let Some(tail) = tail.strip_prefix(".=") else {
        return Err(format!(
            "invalid_arguments: line {line_num}: expected N.=M, N*, <N, >N, or >$; got {rest:?}"
        ));
    };
    let (end, used) = parse_lid(tail, line_num)?;
    if end.saturating_sub(start).saturating_add(1) > MAX_RANGE {
        return Err(format!(
            "invalid_arguments: line {line_num}: range spans more than {MAX_RANGE} lines"
        ));
    }
    Ok((Locator::Range { start, end }, &tail[used..]))
}

fn parse_lid(raw: &str, line_num: u32) -> Result<(u32, usize), String> {
    let bytes = raw.as_bytes();
    if bytes
        .first()
        .is_none_or(|byte| !matches!(byte, b'1'..=b'9'))
    {
        return Err(format!(
            "invalid_arguments: line {line_num}: expected a 1-indexed line number"
        ));
    }
    let mut end = 1;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    let line: u32 = raw[..end]
        .parse()
        .map_err(|_| format!("invalid_arguments: line {line_num}: line number is too large"))?;
    Ok((line, end))
}

fn unquote_path(raw: &str) -> Option<String> {
    if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        return Some(raw[1..raw.len() - 1].to_string());
    }
    if raw.starts_with('"') || raw.starts_with('\'') {
        None
    } else {
        Some(raw.to_string())
    }
}
