//! Canonical hashline patch grammar. Unknown rows and non-canonical range
//! spellings are rejected; omp's parser recovery is not a second syntax.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub path: String,
    /// `[PATH#TAG]` 的快照 tag；`[PATH]`（仅配 ADD）为 None
    pub tag: Option<String>,
    pub ops: Vec<Op>,
    pub file_op: Option<FileOp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Rem,
    Move {
        dest: String,
    },
    /// 新建文件：`+` 正文行（不含换行符）
    Add {
        lines: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub anchor: Anchor,
    pub payload: Payload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    Range { start: u32, end: u32 },
    Block { line: u32 },
    Before { line: u32 },
    After { line: u32 },
    AfterBlock { line: u32 },
    Eof,
    Bof,
}

impl std::fmt::Display for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Range { start, end } => write!(f, "{start}.={end}"),
            Self::Block { line } => write!(f, "{line}*"),
            Self::Before { line } => write!(f, "<{line}"),
            Self::After { line } => write!(f, ">{line}"),
            Self::AfterBlock { line } => write!(f, ">{line}*"),
            Self::Eof => write!(f, ">$"),
            Self::Bof => write!(f, "<1"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Body(Vec<String>),
    Register(Option<String>),
    Cut(Option<String>),
}

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const MAX_RANGE: u32 = 100_000;
const NO_SECTIONS: &str = "invalid_arguments: no hashline sections found in input; copy a \
    [PATH#TAG] header from the latest hashline_read";

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
        return Err(NO_SECTIONS.into());
    }
    let rows = if rows[0].1.trim_end() == BEGIN {
        if rows.last().map(|row| row.1.trim_end()) != Some(END) {
            return Err(
                "invalid_arguments: patch starts with *** Begin Patch but has no *** End Patch; \
                 append `*** End Patch` as the final row"
                    .into(),
            );
        }
        let inner = &rows[1..rows.len() - 1];
        if inner.is_empty() {
            return Err(NO_SECTIONS.into());
        }
        inner
    } else if rows
        .iter()
        .any(|row| row.1.trim_end() == BEGIN || row.1.trim_end() == END)
    {
        return Err(
            "invalid_arguments: *** Begin Patch / *** End Patch must wrap the whole payload; \
             either wrap everything or drop both markers"
                .into(),
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
        if let Some((path, tag)) = parse_header(trimmed).or_else(|| parse_add_header(trimmed)) {
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
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.contains('#') {
            let body = &trimmed[1..trimmed.len() - 1];
            let (path, tag) = body.rsplit_once('#').unwrap_or(("", ""));
            let reason = if tag.len() != 4 {
                format!("tag must be 4 hex characters, got {tag:?}")
            } else if !tag.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                format!("tag {tag:?} is not hexadecimal")
            } else if path.is_empty() {
                "path is empty".to_string()
            } else {
                "path contains `#`".to_string()
            };
            return Err(format!(
                "invalid_arguments: line {line_num}: malformed [PATH#TAG] header ({reason}); \
                 copy the header from the latest hashline_read or hashline_edit output"
            ));
        }
        let Some(section) = current.as_mut() else {
            return Err(format!(
                "invalid_arguments: line {line_num}: payload has no [PATH#TAG] header; start \
                 the section with the header copied from the latest hashline_read"
            ));
        };
        if let Some(header) = parse_hunk(trimmed, *line_num)? {
            section.flush_body(*line_num)?;
            section.push_hunk(header, *line_num)?;
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            let Some(body) = section.body.as_mut() else {
                let hint = match section.ops.last() {
                    Some(
                        op @ Op {
                            payload: Payload::Register(_),
                            ..
                        },
                    ) => format!(
                        "; the preceding {} is a register paste; drop the `+` rows",
                        op_line(op)
                    ),
                    Some(
                        op @ Op {
                            payload: Payload::Cut(_),
                            ..
                        },
                    ) => format!(
                        "; {} takes no body rows; use PUT with `:` for replacement",
                        op_line(op)
                    ),
                    _ => String::new(),
                };
                return Err(format!(
                    "invalid_arguments: line {line_num}: `+` body row has no PUT header{hint}"
                ));
            };
            body.push(text.to_string());
            continue;
        }
        return Err(format!(
            "invalid_arguments: line {line_num}: expected [PATH#TAG], [PATH], PUT/CUT/REM/MV/ADD, or a \
             `+` body row; drop unchanged lines, got {trimmed:?}"
        ));
    }
    if let Some(section) = current.take() {
        sections.push(section.finish(rows.last().map(|row| row.0).unwrap_or(1))?);
    }
    if sections.is_empty() {
        return Err(NO_SECTIONS.into());
    }
    Ok(sections)
}

/// 操作在补丁文本中的原始写法，用于错误信息中的定位提示。
pub(super) fn op_line(op: &Op) -> String {
    match op.payload {
        Payload::Body(_) => format!("PUT {}:", op.anchor),
        Payload::Cut(_) => format!("CUT {}", op.anchor),
        Payload::Register(_) => format!("PUT {} @…", op.anchor),
    }
}

struct OpenSection {
    path: String,
    tag: Option<String>,
    ops: Vec<Op>,
    file_op: Option<FileOp>,
    body: Option<Vec<String>>,
}

impl OpenSection {
    fn flush_body(&mut self, line_num: u32) -> Result<(), String> {
        let Some(body) = self.body.take() else {
            return Ok(());
        };
        if let Some(FileOp::Add { lines }) = self.file_op.as_mut() {
            if body.is_empty() {
                return Err(format!(
                    "invalid_arguments: line {line_num}: ADD requires one or more `+` body rows"
                ));
            }
            *lines = body;
            return Ok(());
        }
        if body.is_empty() {
            let locator = self
                .ops
                .last()
                .map(|op| op.anchor.to_string())
                .unwrap_or_default();
            return Err(format!(
                "invalid_arguments: line {line_num}: PUT with `:` requires one or more `+` body \
                 rows; for an empty replacement use CUT {locator}"
            ));
        }
        let Some(Op {
            payload: Payload::Body(slot),
            ..
        }) = self.ops.last_mut()
        else {
            return Err(format!(
                "invalid_arguments: line {line_num}: unexpected body rows; move the `+` rows under their PUT header"
            ));
        };
        *slot = body;
        Ok(())
    }

    fn push_hunk(&mut self, header: Hunk, line_num: u32) -> Result<(), String> {
        match (&header, self.tag.is_some()) {
            (Hunk::Add, true) => {
                return Err(format!(
                    "invalid_arguments: line {line_num}: ADD creates a new file and belongs under \
                     an untagged [PATH] header, not [PATH#TAG]"
                ));
            }
            (Hunk::Add, false) => {}
            (_, false) => {
                return Err(format!(
                    "invalid_arguments: line {line_num}: a [PATH] section accepts only ADD; \
                     editing an existing file needs the [PATH#TAG] header from hashline_read"
                ));
            }
            _ => {}
        }
        match header {
            Hunk::Add => {
                if self.file_op.is_some() {
                    return Err(format!(
                        "invalid_arguments: line {line_num}: multiple ADD in one section; keep one ADD"
                    ));
                }
                self.file_op = Some(FileOp::Add { lines: Vec::new() });
                self.body = Some(Vec::new());
            }
            Hunk::File(op) => {
                if self.file_op.is_some() {
                    return Err(format!(
                        "invalid_arguments: line {line_num}: multiple REM/MV in one section; keep one REM or MV"
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
            let header = match &self.tag {
                Some(tag) => format!("[{}#{tag}]", self.path),
                None => format!("[{}]", self.path),
            };
            return Err(format!(
                "invalid_arguments: {header} has no operations; add at least one \
                 PUT/CUT/REM/MV (or ADD under [PATH]) row after the header"
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
    Add,
    Op { op: Op, wants_body: bool },
    File(FileOp),
}

fn parse_header(line: &str) -> Option<(String, Option<String>)> {
    let body = line.strip_prefix('[')?.strip_suffix(']')?;
    let (path, tag) = body.rsplit_once('#')?;
    if path.is_empty()
        || path.contains('#')
        || tag.len() != 4
        || !tag.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some((path.to_string(), Some(tag.to_ascii_uppercase())))
}

/// 不带 TAG 的 `[PATH]` 头（仅配 ADD）；含 `#` 的写法归 `[PATH#TAG]` 校验。
fn parse_add_header(line: &str) -> Option<(String, Option<String>)> {
    let path = line.strip_prefix('[')?.strip_suffix(']')?;
    if path.is_empty() || path.contains('#') {
        return None;
    }
    Some((path.to_string(), None))
}

fn parse_hunk(line: &str, line_num: u32) -> Result<Option<Hunk>, String> {
    let line = line.trim();
    if let Some(rest) = keyword_tail(line, "REM") {
        if !rest.trim().is_empty() {
            return Err(format!(
                "invalid_arguments: line {line_num}: REM takes no arguments; drop the stray text"
            ));
        }
        return Ok(Some(Hunk::File(FileOp::Rem)));
    }
    if let Some(rest) = keyword_tail(line, "ADD") {
        if !rest.trim().is_empty() {
            return Err(format!(
                "invalid_arguments: line {line_num}: ADD takes no arguments; put the file text in \
                 `+` rows below it"
            ));
        }
        return Ok(Some(Hunk::Add));
    }
    if let Some(rest) = keyword_tail(line, "MV") {
        let dest = unquote_path(rest.trim()).ok_or_else(|| {
            format!(
                "invalid_arguments: line {line_num}: MV destination is invalid; use \
                 `MV DEST` (quote paths that contain spaces)"
            )
        })?;
        if dest.is_empty() {
            return Err(format!(
                "invalid_arguments: line {line_num}: MV requires a destination path; use \
                 `MV DEST`"
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
    let locator_text = locator.to_string();
    let anchor = match locator {
        Locator::Line { line } => {
            return Err(format!(
                "invalid_arguments: line {line_num}: expected N.=M, N*, <N, >N, or >$; got \
                 \"{line}\""
            ));
        }
        Locator::Anchor(Anchor::Before { line: 1 }) => Anchor::Bof,
        Locator::Anchor(anchor) => anchor,
    };
    let payload = if colon {
        Payload::Body(Vec::new())
    } else {
        if matches!(anchor, Anchor::Range { .. } | Anchor::Block { .. }) && register.is_none() {
            return Err(format!(
                "invalid_arguments: line {line_num}: range/block paste requires a named \
                 @register; use `PUT {locator_text} @name`"
            ));
        }
        Payload::Register(register)
    };
    let op = Op { anchor, payload };
    if let (Anchor::Range { start, end }, Payload::Body(_)) = (&op.anchor, &op.payload)
        && end < start
    {
        return Err(format!(
            "invalid_arguments: line {line_num}: replace_range end is before start; \
             swap to `PUT {end}.={start}:`"
        ));
    }
    Ok(Hunk::Op { op, wants_body })
}

fn parse_cut(rest: &str, line_num: u32) -> Result<Hunk, String> {
    let (locator, register, colon) = split_locator(rest, line_num)?;
    if colon {
        return Err(format!(
            "invalid_arguments: line {line_num}: CUT takes no body rows; drop the trailing `:`"
        ));
    }
    let op = match locator {
        Locator::Line { line } => Op {
            anchor: Anchor::Range {
                start: line,
                end: line,
            },
            payload: Payload::Cut(register),
        },
        Locator::Anchor(Anchor::Range { start, end }) => {
            if end < start {
                return Err(format!(
                    "invalid_arguments: line {line_num}: CUT end is before start; swap to \
                     `CUT {end}.={start}`"
                ));
            }
            Op {
                anchor: Anchor::Range { start, end },
                payload: Payload::Cut(register),
            }
        }
        Locator::Anchor(Anchor::Block { line }) => Op {
            anchor: Anchor::Block { line },
            payload: Payload::Cut(register),
        },
        Locator::Anchor(other) => {
            return Err(format!(
                "invalid_arguments: line {line_num}: CUT requires N, N.=M or N*; got {other}"
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
    Line { line: u32 },
    Anchor(Anchor),
}

impl std::fmt::Display for Locator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Line { line } => write!(f, "{line}"),
            Self::Anchor(anchor) => write!(f, "{anchor}"),
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
                "invalid_arguments: line {line_num}: empty register name; name the register \
                 `@name` (letters, digits, _ or -)"
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
            "invalid_arguments: line {line_num}: trailing junk after locator: {after:?}; \
             body rows start with `+` on their own line"
        ));
    }
    Ok((locator, register, colon))
}

fn parse_locator(rest: &str, line_num: u32) -> Result<(Locator, &str), String> {
    if let Some(after) = rest.strip_prefix('>') {
        let after = after.trim_start();
        if let Some(tail) = after.strip_prefix('$') {
            return Ok((Locator::Anchor(Anchor::Eof), tail));
        }
        let (line, used) = parse_lid(after, line_num)?;
        let tail = &after[used..];
        if let Some(tail) = tail.strip_prefix('*') {
            return Ok((Locator::Anchor(Anchor::AfterBlock { line }), tail));
        }
        return Ok((Locator::Anchor(Anchor::After { line }), tail));
    }
    if let Some(after) = rest.strip_prefix('<') {
        let after = after.trim_start();
        let (line, used) = parse_lid(after, line_num)?;
        return Ok((Locator::Anchor(Anchor::Before { line }), &after[used..]));
    }
    let (start, used) = parse_lid(rest, line_num)?;
    let tail = &rest[used..];
    if let Some(tail) = tail.strip_prefix("*") {
        return Ok((Locator::Anchor(Anchor::Block { line: start }), tail));
    }
    if let Some(tail) = tail.strip_prefix(".=") {
        let (end, used) = parse_lid(tail, line_num)?;
        if end.saturating_sub(start).saturating_add(1) > MAX_RANGE {
            return Err(format!(
                "invalid_arguments: line {line_num}: range spans more than {MAX_RANGE} lines"
            ));
        }
        return Ok((Locator::Anchor(Anchor::Range { start, end }), &tail[used..]));
    }
    if tail.is_empty()
        || tail.starts_with(|c: char| c.is_whitespace())
        || tail.starts_with('@')
        || tail.starts_with(':')
    {
        return Ok((Locator::Line { line: start }, tail));
    }
    Err(format!(
        "invalid_arguments: line {line_num}: expected N.=M, N*, <N, >N, or >$; got {rest:?}"
    ))
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
