use super::Tool;
use super::filesystem::{
    EditPlan, commit_text_edit, format_edit_result, resolve_path, with_edit_locks, write_new_file,
};
use super::names;
use crate::model::ToolInput;
use async_trait::async_trait;
use std::fs;
use std::path::{Path, PathBuf};

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";
const ATAT: &str = "@@";
const ATAT_CONTEXT: &str = "@@ ";

/// Codex 上游 V4A Lark 文法（codex-rs/core/assets/tools/apply_patch.lark）
const V4A_LARK: &str = include_str!("apply_patch.lark");

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

fn parse_hunks(lines: &[&str]) -> Result<Vec<FileHunk>, String> {
    let mut hunks: Vec<FileHunk> = Vec::new();
    let mut ended = false;
    for line in lines.iter().copied().skip(1) {
        if ended {
            return Err(format!(
                "invalid_arguments: trailing content after *** End Patch: {line}"
            ));
        }
        if line == END_PATCH {
            check_update_not_empty(&hunks)?;
            ended = true;
        } else if let Some(path) = line.strip_prefix(ADD_FILE) {
            check_update_not_empty(&hunks)?;
            hunks.push(FileHunk::Add {
                path: path.into(),
                contents: String::new(),
            });
        } else if let Some(path) = line.strip_prefix(DELETE_FILE) {
            check_update_not_empty(&hunks)?;
            hunks.push(FileHunk::Delete { path: path.into() });
        } else if let Some(path) = line.strip_prefix(UPDATE_FILE) {
            check_update_not_empty(&hunks)?;
            hunks.push(FileHunk::Update {
                path: path.into(),
                move_to: None,
                chunks: Vec::new(),
            });
        } else {
            match hunks.last_mut() {
                Some(FileHunk::Add { contents, .. }) => {
                    let Some(rest) = line.strip_prefix('+') else {
                        return Err(format!(
                            "invalid_arguments: Add File lines must start with '+', got: {line}"
                        ));
                    };
                    contents.push_str(rest);
                    contents.push('\n');
                }
                Some(FileHunk::Update {
                    move_to, chunks, ..
                }) => parse_update_line(line, move_to, chunks)?,
                _ => {
                    return Err(format!(
                        "invalid_arguments: expected a file operation or *** End Patch, got: {line}"
                    ));
                }
            }
        }
    }
    if !ended {
        return Err("invalid_arguments: missing *** End Patch".into());
    }
    if hunks.is_empty() {
        return Err("invalid_arguments: patch contains no file operations".into());
    }
    Ok(hunks)
}

/// 上一个 Update hunk 必须带有块或 Move（在下一个 hunk 开始或补丁结束时校验）。
fn check_update_not_empty(hunks: &[FileHunk]) -> Result<(), String> {
    match hunks.last() {
        Some(FileHunk::Update {
            path,
            move_to: None,
            chunks,
        }) if chunks.is_empty() => Err(format!(
            "invalid_arguments: Update file hunk for path '{path}' is empty"
        )),
        _ => Ok(()),
    }
}

fn parse_update_line(
    line: &str,
    move_to: &mut Option<String>,
    chunks: &mut Vec<UpdateChunk>,
) -> Result<(), String> {
    let trimmed = line.trim_end();
    // 上下文 / 删除 / 新增行同时作用于最近的块（无块时忽略）。
    let mut push = |old: Option<&str>, new: Option<&str>| {
        if let Some(chunk) = chunks.last_mut() {
            chunk.old_lines.extend(old.map(str::to_string));
            chunk.new_lines.extend(new.map(str::to_string));
        }
    };
    if trimmed == ATAT {
        chunks.push(UpdateChunk::new(None));
    } else if let Some(ctx) = trimmed.strip_prefix(ATAT_CONTEXT) {
        chunks.push(UpdateChunk::new(Some(ctx.to_string())));
    } else if trimmed == END_OF_FILE {
        // 语法保留：标记本身不再携带语义。上游 V4A 用它锚定 EOF 匹配；
        // vva 匹配本就搜索全文，未命中按 not_found 显式报错，不再静默追加文件尾。
    } else if let Some(rest) = line.strip_prefix('+') {
        push(None, Some(rest));
    } else if let Some(rest) = line.strip_prefix('-') {
        push(Some(rest), None);
    } else if let Some(rest) = line.strip_prefix(' ') {
        push(Some(rest), Some(rest));
    } else if let Some(dest) = trimmed.strip_prefix(MOVE_TO)
        && chunks.is_empty()
        && move_to.is_none()
    {
        *move_to = Some(dest.to_string());
    } else {
        return Err(format!(
            "invalid_arguments: invalid line in Update File hunk: {line}"
        ));
    }
    Ok(())
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
    with_edit_locks(&[&path], || {
        if path.exists() {
            return Err(format!(
                "Failed to create file {}: already exists",
                path.display()
            ));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("Failed to create directory {}: {error}", parent.display())
            })?;
        }
        write_new_file(&path, contents)?;
        Ok(format!("Created file {}", path.display()))
    })
}

fn delete_file(working_dir: &Path, path: &str) -> Result<String, String> {
    let path = resolve_path(working_dir, path);
    let path = fs::canonicalize(&path)
        .map_err(|e| format!("Failed to resolve {}: {e}", path.display()))?;
    with_edit_locks(&[&path], || {
        fs::remove_file(&path).map_err(|e| format!("Failed to delete {}: {e}", path.display()))?;
        Ok(format!("Deleted file {}", path.display()))
    })
}

fn update_file(
    working_dir: &Path,
    path: &str,
    move_to: Option<&str>,
    chunks: &[UpdateChunk],
) -> Result<String, String> {
    let src = resolve_path(working_dir, path);
    let apply = |content: &str| {
        if chunks.is_empty() {
            Ok(content.to_string())
        } else {
            apply_update(content, chunks)
        }
    };
    let Some(dest_rel) = move_to else {
        return commit_text_edit(&src, None, false, |content| {
            Ok(EditPlan::from_replacement(content, apply(content)?))
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
    with_edit_locks(&[&src, &dest], || {
        let content = fs::read_to_string(&src)
            .map_err(|e| format!("Failed to read {}: {e}", src.display()))?;
        if content.contains('\0') {
            return Err("Unsupported binary file".into());
        }
        let updated = apply(&content)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("Failed to create directory {}: {error}", parent.display())
            })?;
        }
        write_new_file(&dest, &updated)?;
        fs::remove_file(&src).map_err(|e| format!("Failed to remove {}: {e}", src.display()))?;
        let plan = EditPlan::from_replacement(&content, updated);
        Ok(format!(
            "Moved {} -> {}\n{}",
            src.display(),
            dest.display(),
            format_edit_result(&dest, &content, &plan, false)
        ))
    })
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        names::APPLY_PATCH
    }

    fn description(&self) -> &str {
        r#"Edit files with a patch. The patch language is a stripped-down, file-oriented diff format, wrapped in an envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Each file section starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

An Update section may start with `*** Move to: <new_path>` to rename the file, followed by one or more hunks. A hunk starts with `@@` or `@@ <context>` (a line such as a function or class header that locates the hunk). Each hunk line starts with a space (unchanged context), `-` (removed) or `+` (added). `*** End of File` after the last hunk marks a change at the end of the file.

Example:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

Remember:
- Every section needs an Add/Delete/Update header.
- Prefix new lines with `+` even when creating a new file.
- Paths are absolute or workspace-relative; shell variables and ~ are not expanded.
- One call may include several file sections."#
    }

    fn input(&self) -> ToolInput {
        ToolInput::Freeform {
            grammar: Some(V4A_LARK.to_string()),
        }
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let hunks = parse_apply_patch(arguments)?;
        let working_dir = self.working_dir.clone();
        tokio::task::spawn_blocking(move || apply_parsed(&working_dir, hunks))
            .await
            .map_err(|e| format!("apply_patch task failed: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    fn patch_input(body: &str) -> String {
        format!("*** Begin Patch\n{body}*** End Patch")
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
    fn parse_rejects_environment_id_and_heredoc() {
        assert!(
            parse_apply_patch(
                "*** Begin Patch\n*** Environment ID: remote\n*** Add File: a\n+x\n*** End Patch"
            )
            .is_err()
        );
        assert!(
            parse_apply_patch("<<EOF\n*** Begin Patch\n*** Delete File: a\n*** End Patch\nEOF")
                .is_err()
        );
    }

    #[test]
    fn definition_is_freeform_with_grammar() {
        let tool = ApplyPatchTool::new(".");
        assert_eq!(tool.name(), names::APPLY_PATCH);
        match tool.input() {
            ToolInput::Freeform { grammar } => {
                assert!(grammar.unwrap().starts_with("start: begin_patch hunk+"))
            }
            ToolInput::Json(_) => panic!("expected freeform"),
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
    async fn apply_add_creates_parent_directories() {
        let root = tempfile::tempdir().unwrap();
        let tool = ApplyPatchTool::new(root.path());
        let patch = patch_input("*** Add File: nested/deep/new.txt\n+hello\n");
        tool.execute(&patch).await.unwrap();
        let dest = root.path().join("nested/deep/new.txt");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "hello\n");
        assert!(tool.execute(&patch).await.is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "hello\n");
    }

    #[tokio::test]
    async fn apply_move_creates_parent_directories() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("source.txt");
        fs::write(&src, "hello\n").unwrap();
        let tool = ApplyPatchTool::new(root.path());
        let patch = patch_input(
            "*** Update File: source.txt\n*** Move to: nested/deep/moved.txt\n@@\n-hello\n+updated\n",
        );
        tool.execute(&patch).await.unwrap();
        let dest = root.path().join("nested/deep/moved.txt");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "updated\n");
        assert!(!src.exists());

        fs::write(&src, "hello\n").unwrap();
        assert!(tool.execute(&patch).await.is_err());
        assert_eq!(fs::read_to_string(&src).unwrap(), "hello\n");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "updated\n");
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
