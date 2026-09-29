use super::Tool;
use super::filesystem::{
    EditPlan, commit_text_edit, edit_lock, format_edit_result, parse_args, resolve_path,
    write_new_file,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";
const ENVIRONMENT_ID: &str = "*** Environment ID: ";
const ATAT: &str = "@@";
const ATAT_CONTEXT: &str = "@@ ";

pub struct ApplyPatchTool {
    working_dir: PathBuf,
}

impl ApplyPatchTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    input: String,
}

#[derive(Debug, PartialEq, Eq)]
enum FileHunk {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpdateChunk {
    change_context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

impl UpdateChunk {
    fn new(change_context: Option<String>) -> Self {
        Self {
            change_context,
            old_lines: Vec::new(),
            new_lines: Vec::new(),
        }
    }
}

fn parse_apply_patch(input: &str) -> Result<Vec<FileHunk>, String> {
    let lines: Vec<&str> = input.trim().lines().collect();
    let lines = strip_optional_heredoc(&lines);
    if lines.first().copied() != Some(BEGIN_PATCH) {
        return Err(
            "invalid_arguments: patch must start with *** Begin Patch and end with *** End Patch"
                .into(),
        );
    }
    if lines.last().copied() != Some(END_PATCH) {
        return Err("invalid_arguments: the last line of the patch must be *** End Patch".into());
    }
    parse_hunks(&lines)
}

fn strip_optional_heredoc<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    if lines.len() >= 4
        && is_heredoc_start(lines[0])
        && lines.last().is_some_and(|line| line.ends_with("EOF"))
    {
        lines[1..lines.len() - 1].to_vec()
    } else {
        lines.to_vec()
    }
}

fn is_heredoc_start(line: &str) -> bool {
    matches!(line.trim(), "<<EOF" | "<<'EOF'" | "<<\"EOF\"")
}

enum ParseState {
    Header {
        saw_env: bool,
    },
    Body,
    Add {
        path: String,
        contents: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
    End,
}

fn parse_hunks(lines: &[&str]) -> Result<Vec<FileHunk>, String> {
    let mut hunks = Vec::new();
    let mut state = ParseState::Header { saw_env: false };
    for line in lines.iter().copied().skip(1) {
        state = consume_line(state, line, &mut hunks)?;
    }
    match state {
        ParseState::End => Ok(hunks),
        _ => Err("invalid_arguments: missing *** End Patch".into()),
    }
}

fn consume_line(
    state: ParseState,
    line: &str,
    hunks: &mut Vec<FileHunk>,
) -> Result<ParseState, String> {
    if let Some(next) = start_hunk_or_end(&state, line, hunks)? {
        return Ok(next);
    }
    match state {
        ParseState::Header { saw_env } => {
            if let Some(id) = line.strip_prefix(ENVIRONMENT_ID) {
                if saw_env {
                    return Err("invalid_arguments: duplicate *** Environment ID: preamble".into());
                }
                if id.is_empty() {
                    return Err(
                        "invalid_arguments: apply_patch environment_id cannot be empty".into(),
                    );
                }
                return Ok(ParseState::Header { saw_env: true });
            }
            Err(invalid_hunk_header(line))
        }
        ParseState::Body => Err(invalid_hunk_header(line)),
        ParseState::Add { path, mut contents } => {
            let Some(rest) = line.strip_prefix('+') else {
                return Err(format!(
                    "invalid_arguments: Add File lines must start with '+', got: {line}"
                ));
            };
            contents.push_str(rest);
            contents.push('\n');
            Ok(ParseState::Add { path, contents })
        }
        ParseState::Update {
            path,
            mut move_to,
            mut chunks,
        } => {
            let update_line = line.trim_end();
            if update_line == ATAT {
                chunks.push(UpdateChunk::new(None));
            } else if let Some(ctx) = update_line.strip_prefix(ATAT_CONTEXT) {
                chunks.push(UpdateChunk::new(Some(ctx.to_string())));
            } else if update_line == END_OF_FILE {
                // 语法保留：标记本身不再携带语义。上游 V4A 用它锚定 EOF 匹配；
                // vva 匹配本就搜索全文，未命中按 not_found 显式报错，不再静默追加文件尾。
            } else if update_line.is_empty() {
                push_context(&mut chunks, String::new());
            } else if let Some(rest) = line.strip_prefix('+') {
                push_add(&mut chunks, rest.to_string());
            } else if let Some(rest) = line.strip_prefix('-') {
                push_delete(&mut chunks, rest.to_string());
            } else if let Some(rest) = line.strip_prefix(' ') {
                push_context(&mut chunks, rest.to_string());
            } else if let Some(dest) = update_line.strip_prefix(MOVE_TO)
                && chunks.is_empty()
                && move_to.is_none()
            {
                move_to = Some(dest.to_string());
            } else {
                return Err(format!(
                    "invalid_arguments: invalid line in Update File hunk: {line}"
                ));
            }
            Ok(ParseState::Update {
                path,
                move_to,
                chunks,
            })
        }
        ParseState::End => Err(format!(
            "invalid_arguments: trailing content after *** End Patch: {line}"
        )),
    }
}

fn start_hunk_or_end(
    state: &ParseState,
    line: &str,
    hunks: &mut Vec<FileHunk>,
) -> Result<Option<ParseState>, String> {
    if line == END_PATCH {
        flush_state(state, hunks)?;
        return Ok(Some(ParseState::End));
    }
    if let Some(path) = line.strip_prefix(ADD_FILE) {
        flush_state(state, hunks)?;
        return Ok(Some(ParseState::Add {
            path: path.to_string(),
            contents: String::new(),
        }));
    }
    if let Some(path) = line.strip_prefix(DELETE_FILE) {
        flush_state(state, hunks)?;
        hunks.push(FileHunk::Delete {
            path: path.to_string(),
        });
        return Ok(Some(ParseState::Body));
    }
    if let Some(path) = line.strip_prefix(UPDATE_FILE) {
        flush_state(state, hunks)?;
        return Ok(Some(ParseState::Update {
            path: path.to_string(),
            move_to: None,
            chunks: Vec::new(),
        }));
    }
    Ok(None)
}

fn flush_state(state: &ParseState, hunks: &mut Vec<FileHunk>) -> Result<(), String> {
    match state {
        ParseState::Header { .. } | ParseState::Body | ParseState::End => Ok(()),
        ParseState::Add { path, contents } => {
            hunks.push(FileHunk::Add {
                path: path.clone(),
                contents: contents.clone(),
            });
            Ok(())
        }
        ParseState::Update {
            path,
            move_to,
            chunks,
        } => {
            if chunks.is_empty() && move_to.is_none() {
                return Err(format!(
                    "invalid_arguments: Update file hunk for path '{path}' is empty"
                ));
            }
            hunks.push(FileHunk::Update {
                path: path.clone(),
                move_to: move_to.clone(),
                chunks: chunks.clone(),
            });
            Ok(())
        }
    }
}

fn invalid_hunk_header(line: &str) -> String {
    format!("invalid_arguments: expected a file operation or *** End Patch, got: {line}")
}

fn push_add(chunks: &mut [UpdateChunk], line: String) {
    if let Some(chunk) = chunks.last_mut() {
        chunk.new_lines.push(line);
    }
}

fn push_delete(chunks: &mut [UpdateChunk], line: String) {
    if let Some(chunk) = chunks.last_mut() {
        chunk.old_lines.push(line);
    }
}

fn push_context(chunks: &mut [UpdateChunk], line: String) {
    if let Some(chunk) = chunks.last_mut() {
        chunk.old_lines.push(line.clone());
        chunk.new_lines.push(line);
    }
}

fn apply_update(original: &str, chunks: &[UpdateChunk]) -> Result<String, String> {
    let mut lines: Vec<String> = original.split('\n').map(str::to_string).collect();
    let replacements = compute_replacements(&lines, chunks)?;
    for (start, old_len, new_lines) in replacements.into_iter().rev() {
        lines.splice(start..start + old_len, new_lines);
    }
    if lines.last().is_none_or(|line| !line.is_empty()) {
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

fn compute_replacements(
    original_lines: &[String],
    chunks: &[UpdateChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>, String> {
    let mut replacements = Vec::new();
    let mut line_index = 0;
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(ctx) = &chunk.change_context {
            let Some(idx) = seek_sequence(original_lines, std::slice::from_ref(ctx), line_index)
            else {
                return Err(format!(
                    "hunk {}: not_found: @@ context `{ctx}` was not found. Read the file and retry.",
                    i + 1
                ));
            };
            line_index = idx + 1;
        }
        if chunk.old_lines.is_empty() {
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }
        let Some(start) = seek_sequence(original_lines, &chunk.old_lines, line_index) else {
            return Err(format!(
                "hunk {}: not_found: hunk context did not match. Read the current file and copy exact lines.",
                i + 1
            ));
        };
        replacements.push((start, chunk.old_lines.len(), chunk.new_lines.clone()));
        line_index = start + chunk.new_lines.len();
    }
    Ok(replacements)
}

/// 匹配语义对齐上游 openai/codex `codex-rs/apply-patch/src/seek_sequence.rs`：
/// 逐级放宽（精确 → 忽略行尾空白 → 两侧 trim → Unicode 标点归一化），
/// 上游注释称最后一级 "mirrors the fuzzy behaviour of `git apply"`。
/// vva 扩展：start 起未命中时回退从 0 重搜（容忍乱序 hunk，仍要求真实命中）。
fn seek_sequence(lines: &[String], pattern: &[String], start: usize) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start.min(lines.len()));
    }
    if let Some(idx) = seek_from(lines, pattern, start) {
        return Some(idx);
    }
    if start > 0
        && let Some(idx) = seek_from(lines, pattern, 0)
    {
        return Some(idx);
    }
    None
}

fn seek_from(lines: &[String], pattern: &[String], start: usize) -> Option<usize> {
    let haystack = &lines[start.min(lines.len())..];
    if let Some(rel) = find_first(haystack, pattern) {
        return Some(start + rel);
    }
    find_first_mapped(lines, pattern, start, |line| rtrim(line).to_string())
        .or_else(|| find_first_mapped(lines, pattern, start, |line| line.trim().to_string()))
        .or_else(|| {
            find_first_mapped(lines, pattern, start, |line| {
                normalize_unicode_punctuation(rtrim(line))
            })
        })
}

fn find_first(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    (0..=last).find(|&i| haystack[i..i + needle.len()] == needle[..])
}

fn find_first_mapped(
    lines: &[String],
    pattern: &[String],
    start: usize,
    map: impl Fn(&str) -> String,
) -> Option<usize> {
    let hay: Vec<String> = lines[start.min(lines.len())..]
        .iter()
        .map(|line| map(line))
        .collect();
    let needle: Vec<String> = pattern.iter().map(|line| map(line)).collect();
    find_first(&hay, &needle).map(|rel| start + rel)
}

fn rtrim(line: &str) -> &str {
    line.trim_end_matches([' ', '\t'])
}

fn normalize_unicode_punctuation(s: &str) -> String {
    s.chars()
        .map(|ch| {
            if ('\u{FF01}'..='\u{FF5E}').contains(&ch) {
                return char::from_u32(u32::from(ch) - 0xFEE0).unwrap_or(ch);
            }
            match ch {
                '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2007}' | '\u{2008}' | '\u{2009}'
                | '\u{200A}' | '\u{202F}' => ' ',
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                | '\u{2212}' | '\u{FE58}' | '\u{FE63}' => '-',
                '\u{3001}' => ',',
                '\u{3002}' | '\u{00B7}' | '\u{2022}' => '.',
                other => other,
            }
        })
        .collect()
}

fn apply_parsed(working_dir: &Path, hunks: Vec<FileHunk>) -> Result<String, String> {
    if hunks.is_empty() {
        return Ok("No file operations in patch".into());
    }
    let mut reports = Vec::new();
    for hunk in hunks {
        match hunk {
            FileHunk::Add { path, contents } => {
                reports.push(add_file(working_dir, &path, &contents)?);
            }
            FileHunk::Delete { path } => {
                reports.push(delete_file(working_dir, &path)?);
            }
            FileHunk::Update {
                path,
                move_to,
                chunks,
            } => {
                reports.push(update_file(
                    working_dir,
                    &path,
                    move_to.as_deref(),
                    &chunks,
                )?);
            }
        }
    }
    Ok(reports.join("\n\n"))
}

fn add_file(working_dir: &Path, path: &str, contents: &str) -> Result<String, String> {
    let path = resolve_path(working_dir, path);
    if path.exists() {
        return Err(format!(
            "Failed to create file {}: already exists",
            path.display()
        ));
    }
    let lock = edit_lock(&path)?;
    let _guard = lock
        .lock()
        .map_err(|_| "File mutation lock poisoned".to_string())?;
    if path.exists() {
        return Err(format!(
            "Failed to create file {}: already exists",
            path.display()
        ));
    }
    write_new_file(&path, contents)?;
    Ok(format!("Created file {}", path.display()))
}

fn delete_file(working_dir: &Path, path: &str) -> Result<String, String> {
    let path = resolve_path(working_dir, path);
    let path = fs::canonicalize(&path)
        .map_err(|e| format!("Failed to resolve {}: {e}", path.display()))?;
    let lock = edit_lock(&path)?;
    let _guard = lock
        .lock()
        .map_err(|_| "File mutation lock poisoned".to_string())?;
    fs::remove_file(&path).map_err(|e| format!("Failed to delete {}: {e}", path.display()))?;
    Ok(format!("Deleted file {}", path.display()))
}

fn update_file(
    working_dir: &Path,
    path: &str,
    move_to: Option<&str>,
    chunks: &[UpdateChunk],
) -> Result<String, String> {
    let src = resolve_path(working_dir, path);
    if chunks.is_empty() && move_to.is_none() {
        return Err("invalid_arguments: Update File has no hunks".into());
    }
    let Some(dest_rel) = move_to else {
        return commit_text_edit(&src, None, false, |content| {
            Ok(EditPlan::from_replacement(
                content,
                apply_update(content, chunks)?,
            ))
        });
    };
    let dest = resolve_path(working_dir, dest_rel);
    if dest.exists() {
        return Err(format!(
            "Failed to move {} -> {}: destination exists",
            src.display(),
            dest.display()
        ));
    }
    let src =
        fs::canonicalize(&src).map_err(|e| format!("Failed to resolve {}: {e}", src.display()))?;
    let src_lock = edit_lock(&src)?;
    let dest_lock = edit_lock(&dest)?;
    let _g1 = src_lock
        .lock()
        .map_err(|_| "File mutation lock poisoned".to_string())?;
    let _g2 = dest_lock
        .lock()
        .map_err(|_| "File mutation lock poisoned".to_string())?;
    let content =
        fs::read_to_string(&src).map_err(|e| format!("Failed to read {}: {e}", src.display()))?;
    if content.contains('\0') {
        return Err("Unsupported binary file".into());
    }
    let updated = if chunks.is_empty() {
        content.clone()
    } else {
        apply_update(&content, chunks)?
    };
    write_new_file(&dest, &updated)?;
    fs::remove_file(&src).map_err(|e| format!("Failed to remove {}: {e}", src.display()))?;
    let plan = EditPlan::from_replacement(&content, updated);
    Ok(format!(
        "Moved {} -> {}\n{}",
        src.display(),
        dest.display(),
        format_edit_result(&dest, &content, &plan, false)
    ))
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        r#"Edit files with a V4A patch. The input MUST start with *** Begin Patch and end with *** End Patch. One call may include several file operations. An optional *** Environment ID: line may follow Begin Patch.

*** Add File: path
+line

*** Delete File: path

*** Update File: path
@@
 context
-removed
+added

*** Update File: old_path
*** Move to: new_path
@@
 context

Hunks start with @@ or @@ <context>. Context lines begin with a space, removals with -, additions with +. *** End of File marks an insertion at EOF. Paths are absolute or workspace-relative; shell variables and ~ are not expanded. Example: {"input":"*** Begin Patch\n*** Update File: src/main.rs\n@@\n fn main() {\n-    println!(\"hi\");\n+    println!(\"hello\");\n*** End Patch"}."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Complete V4A patch beginning with *** Begin Patch and ending with *** End Patch."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: ApplyPatchArgs = parse_args(arguments, "Edit")?;
        let hunks = parse_apply_patch(&args.input)?;
        let working_dir = self.working_dir.clone();
        tokio::task::spawn_blocking(move || apply_parsed(&working_dir, hunks))
            .await
            .map_err(|e| format!("Edit task failed: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    fn patch_input(body: &str) -> String {
        serde_json::json!({"input": format!("*** Begin Patch\n{body}*** End Patch")}).to_string()
    }

    #[test]
    fn parse_update_add_delete() {
        let hunks = parse_apply_patch(
            "*** Begin Patch\n*** Update File: a.rs\n@@\n fn main() {\n-    hi\n+    hello\n*** Add File: b.rs\n+ok\n*** Delete File: c.rs\n*** End Patch",
        )
        .unwrap();
        assert_eq!(hunks.len(), 3);
        match &hunks[0] {
            FileHunk::Update { path, chunks, .. } => {
                assert_eq!(path, "a.rs");
                assert_eq!(chunks[0].old_lines, ["fn main() {", "    hi"]);
                assert_eq!(chunks[0].new_lines, ["fn main() {", "    hello"]);
            }
            _ => panic!("expected update"),
        }
        match &hunks[1] {
            FileHunk::Add { path, contents } => {
                assert_eq!(path, "b.rs");
                assert_eq!(contents, "ok\n");
            }
            _ => panic!("expected add"),
        }
        match &hunks[2] {
            FileHunk::Delete { path } => assert_eq!(path, "c.rs"),
            _ => panic!("expected delete"),
        }
    }

    #[test]
    fn parse_environment_id_and_heredoc() {
        let hunks = parse_apply_patch(
            "*** Begin Patch\n*** Environment ID: remote\n*** Add File: hello.txt\n+hello\n*** End Patch",
        )
        .unwrap();
        match &hunks[0] {
            FileHunk::Add { path, contents } => {
                assert_eq!(path, "hello.txt");
                assert_eq!(contents, "hello\n");
            }
            _ => panic!("expected add"),
        }
        let hunks = parse_apply_patch(
            "<<EOF\n*** Begin Patch\n*** Delete File: gone.rs\n*** End Patch\nEOF",
        )
        .unwrap();
        match &hunks[0] {
            FileHunk::Delete { path } => assert_eq!(path, "gone.rs"),
            _ => panic!("expected delete"),
        }
    }

    #[test]
    fn seek_matches_unicode_quotes() {
        let lines = vec!["let s = \u{201C}hi\u{201D};".into()];
        let pattern = vec!["let s = \"hi\";".into()];
        assert_eq!(seek_sequence(&lines, &pattern, 0), Some(0));
    }

    #[test]
    fn parse_rejects_unprefixed_hunk_line() {
        let err = parse_apply_patch(
            "*** Begin Patch\n*** Update File: a.rs\n@@\nfn main() {\n*** End Patch",
        )
        .unwrap_err();
        assert!(err.contains("invalid line"), "{err}");
    }

    #[tokio::test]
    async fn apply_update_first_match_and_lf() {
        let root = std::env::temp_dir().join(format!("vva-patch-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "alpha\r\nbeta\r\n").unwrap();
        let tool = ApplyPatchTool::new(&root);
        tool.execute(&patch_input(
            "*** Update File: sample\n@@\n alpha\n-beta\n+gamma\n",
        ))
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sample")).unwrap(),
            "alpha\ngamma\n"
        );
        fs::write(root.join("dup"), "x\nx\n").unwrap();
        tool.execute(&patch_input("*** Update File: dup\n@@\n-x\n+y\n"))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("dup")).unwrap(), "y\nx\n");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn apply_empty_old_lines_appends_at_eof() {
        let root = std::env::temp_dir().join(format!("vva-patch-ctx-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("sample"), "alpha\nbeta\n").unwrap();
        let tool = ApplyPatchTool::new(&root);
        tool.execute(&patch_input("*** Update File: sample\n@@ alpha\n+mid\n"))
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sample")).unwrap(),
            "alpha\nbeta\nmid\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn apply_add_and_delete() {
        let root = std::env::temp_dir().join(format!("vva-patch-ad-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let tool = ApplyPatchTool::new(&root);
        tool.execute(&patch_input("*** Add File: new.txt\n+hello\n"))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("new.txt")).unwrap(), "hello\n");
        tool.execute(&patch_input("*** Delete File: new.txt\n"))
            .await
            .unwrap();
        assert!(!root.join("new.txt").exists());
        let _ = fs::remove_dir_all(root);
    }
}
