use base64::Engine;
use std::fs;

use super::edit::*;
use super::glob::*;
use super::grep::*;
use super::read::*;
use super::write::*;
use super::*;
use crate::config::resolver::InputCapabilities;
use crate::model::{MediaKind, ToolCall, ToolResult};

#[test]
fn read_paginates_whole_lines_and_long_utf8_lines() {
    let args = ReadArgs {
        path: "sample".into(),
        offset: None,
        limit: Some(1),
        byte_offset: None,
    };
    let output = read_text("alpha\nbeta", &args).unwrap();
    assert!(output.contains("1: alpha\n[Continue with Read offset=2.]"));
    assert!(output.contains(&content_version("alpha\nbeta")));
    let text = "界".repeat(super::super::MAX_INLINE_OUTPUT_BYTES);
    let mut restored = String::new();
    let mut byte_offset = 0;
    loop {
        let page = read_text(
            &text,
            &ReadArgs {
                byte_offset: Some(byte_offset),
                path: args.path.clone(),
                ..args
            },
        )
        .unwrap();
        assert!(page.len() <= super::super::MAX_INLINE_OUTPUT_BYTES);
        let (_, code) = page.lines().nth(1).unwrap().split_once(": ").unwrap();
        restored.push_str(code);
        byte_offset += code.len();
        if byte_offset == text.len() {
            assert!(page.contains("[End of file]"));
            break;
        }
        assert!(page.contains(&format!("byte_offset={byte_offset}, limit=1")));
    }
    assert_eq!(restored, text);
    assert!(
        read_text(
            &text,
            &ReadArgs {
                byte_offset: Some(1),
                path: args.path.clone(),
                ..args
            }
        )
        .is_err()
    );
    let content = format!("{}\n{}", "a".repeat(20000), "b".repeat(20000));
    let page = read_text(
        &content,
        &ReadArgs {
            limit: None,
            path: args.path.clone(),
            ..args
        },
    )
    .unwrap();
    assert!(page.contains("offset=2"));
    assert!(!page.contains("2: b"));
    assert!(read_text("", &args).unwrap().ends_with("[End of file]"));
    assert!(
        read_text(
            "a",
            &ReadArgs {
                offset: Some(0),
                path: args.path.clone(),
                ..args
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn read_binary_is_gated_and_persists_bytes() {
    let root = std::env::temp_dir().join(format!("vva-media-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=";
    fs::write(
        root.join("sample.png"),
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap(),
    )
    .unwrap();
    let call = ToolCall::new("image_read", "Read", r#"{"path":"sample.png"}"#);
    assert!(ReadTool::new(&root).execute_result(&call).await.is_error);
    let registry = crate::tools::ToolRegistry::with_workspace_tools(
        &root,
        Some(root.join("outputs")),
        crate::model::EditorKind::Replace,
        crate::tools::WorkspaceToolDeps {
            inputs: InputCapabilities {
                model: "test/vision".into(),
                modalities: vec!["image".into()],
                protocol: crate::config::ProviderType::Response,
            },
            ..Default::default()
        },
    );
    let result = registry.execute_call(&call).await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(result.attachments[0].data, data);
    assert_eq!(result.attachments[0].kind, MediaKind::Image);
    let restored: ToolResult =
        serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert_eq!(restored, result);
}

#[tokio::test]
async fn edit_preserves_crlf_and_requires_version_for_overwrite() {
    let root = std::env::temp_dir().join(format!("vva-edit-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("sample"), "alpha\r\nbeta\r\n").unwrap();
    let tool = EditTool::new(&root);
    tool.execute(r#"{"path":"sample","old_string":"alpha\nbeta","new_string":"gamma\r\ndelta"}"#)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("sample")).unwrap(),
        "gamma\r\ndelta\r\n"
    );
    assert!(
        tool.execute(r#"{"path":"sample","old_string":"","new_string":"reset"}"#)
            .await
            .unwrap_err()
            .contains("expected_version")
    );
    for replacement in ["reset\r\n", ""] {
        let version = content_version(&fs::read_to_string(root.join("sample")).unwrap());
        tool.execute(&serde_json::json!({"path":"sample", "old_string":"", "new_string":replacement, "expected_version":version}).to_string()).await.unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sample")).unwrap(),
            replacement
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn searches_sort_before_capping_and_only_mark_excess() {
    let root = std::env::temp_dir().join(format!("vva-search-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    for index in 0..=100 {
        let path = root.join(format!("{index:03}.txt"));
        fs::write(&path, if index == 0 { "absent" } else { "match" }).unwrap();
        fs::File::open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(index))
            .unwrap();
    }
    let glob = GlobTool::new(&root)
        .execute(r#"{"pattern":"*.txt"}"#)
        .await
        .unwrap();
    assert!(glob.lines().next().unwrap().ends_with("100.txt"));
    assert!(glob.lines().nth(99).unwrap().ends_with("001.txt"));
    assert_eq!(glob.lines().last(), Some("[results truncated]"));
    let tool = GrepTool::new(&root);
    let grep = tool.execute(r#"{"pattern":"match"}"#).await.unwrap();
    assert_eq!(grep.lines().count(), 100);
    assert!(grep.lines().next().unwrap().ends_with("100.txt:1: match"));
    let path = root.join("000.txt");
    fs::write(&path, "match").unwrap();
    fs::File::open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    let grep = tool.execute(r#"{"pattern":"match"}"#).await.unwrap();
    assert_eq!(grep.lines().count(), 101);
    assert_eq!(grep.lines().last(), Some("[results truncated]"));
    fs::remove_dir_all(root).unwrap();
}

/// 在临时目录中按 (相对路径, 内容) 建文件。
fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    root
}

#[tokio::test]
async fn glob_handles_quotes_and_brace_alternatives() {
    let root = workspace(&[
        ("src/lib.rs", "pub fn example() {}"),
        ("src/readme.md", "# readme"),
        ("src/lib.txt", "example"),
    ]);
    let rs = root.path().join("src/lib.rs").display().to_string();
    let tool = GlobTool::new(root.path());
    assert_eq!(tool.execute(r#"{"pattern":"**/*.rs"}"#).await.unwrap(), rs);
    assert_eq!(
        tool.execute(r#"{"pattern":"\"**/*.rs\""}"#).await.unwrap(),
        rs
    );
    let result = tool.execute(r#"{"pattern":"**/*.{rs,md}"}"#).await.unwrap();
    let mut lines: Vec<_> = result.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            rs.as_str(),
            &root.path().join("src/readme.md").display().to_string()
        ]
    );
}

#[test]
fn strip_wrapping_quotes_peels_paired_quotes() {
    assert_eq!(strip_wrapping_quotes(r#""src/lib.rs""#), "src/lib.rs");
    assert_eq!(strip_wrapping_quotes("'src/lib.rs'"), "src/lib.rs");
    assert_eq!(strip_wrapping_quotes(r#""'src/lib.rs'""#), "src/lib.rs");
    assert_eq!(strip_wrapping_quotes("src/lib.rs"), "src/lib.rs");
    assert_eq!(strip_wrapping_quotes(r#""src/lib.rs"#), r#""src/lib.rs"#);
}

#[tokio::test]
async fn read_strips_quoted_path() {
    let root = workspace(&[("src/lib.rs", "hello")]);
    let result = ReadTool::new(root.path())
        .execute(r#"{"path":"\"src/lib.rs\""}"#)
        .await
        .unwrap();
    assert!(result.ends_with("1: hello\n[End of file]"));
}

#[tokio::test]
async fn read_and_grep_hashline_emit_snapshot_header_and_line_numbers() {
    let root = workspace(&[("sample", "alpha\nbeta\nalpha\n")]);
    let hashline = crate::model::EditorKind::Hashline;
    let result = ReadTool::new(root.path())
        .with_editor(hashline)
        .execute(r#"{"path":"sample"}"#)
        .await
        .unwrap();
    let header = result.lines().next().unwrap();
    assert!(header.starts_with("[sample#"), "{header}");
    assert_eq!(header.len(), "[sample#ABCD]".len(), "{header}");
    for line in ["1:alpha", "2:beta", "3:alpha"] {
        assert!(result.contains(line), "{result}");
    }
    assert!(!result.contains("1: ") && !result.contains("[version:"));

    let grep = GrepTool::new(root.path()).with_editor(hashline);
    let result = grep.execute(r#"{"pattern":"alpha"}"#).await.unwrap();
    let lines: Vec<_> = result.lines().collect();
    assert!(lines[0].starts_with("[sample#"), "{}", lines[0]);
    assert_eq!(lines[1..], ["1:alpha", "3:alpha"]);
    let plain = GrepTool::new(root.path())
        .execute(r#"{"pattern":"alpha"}"#)
        .await
        .unwrap();
    assert!(plain.contains(":1: alpha"));
}

#[test]
fn expand_braces_splits_comma_groups() {
    assert_eq!(
        expand_braces("**/*.{rs,md,toml}").unwrap(),
        ["**/*.rs", "**/*.md", "**/*.toml"]
    );
    assert_eq!(expand_braces("Cargo.toml").unwrap(), ["Cargo.toml"]);
    assert_eq!(expand_braces("{a}").unwrap(), ["{a}"]);
    assert_eq!(expand_braces("{a,{b,c}}").unwrap(), ["a", "b", "c"]);
}

#[tokio::test]
async fn grep_include_expands_braces() {
    let root = workspace(&[
        ("src/lib.rs", "needle in rust"),
        ("src/readme.md", "needle in markdown"),
        ("src/lib.txt", "needle in text"),
    ]);
    let result = GrepTool::new(root.path())
        .execute(r#"{"pattern":"needle","include":"**/*.{rs,md}"}"#)
        .await
        .unwrap();
    assert!(result.contains("src/lib.rs") && result.contains("src/readme.md"));
    assert!(!result.contains("src/lib.txt"));
}

#[tokio::test]
async fn edit_basics_diff_replace_all_and_unchanged() {
    let root = workspace(&[
        ("file.txt", "one\nbefore\nthree\n"),
        ("all.txt", "before before"),
    ]);
    let tool = EditTool::new(root.path());
    let path = root.path().join("file.txt");
    let result = tool
        .execute(r#"{"path":"file.txt","old_string":"before","new_string":"after"}"#)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "one\nafter\nthree\n");
    assert_eq!(
        result,
        format!(
            "--- {0}\n+++ {0}\n@@ -1,3 +1,3 @@\n one\n-before\n+after\n three\n[version: {1}]",
            path.display(),
            content_version("one\nafter\nthree\n")
        )
    );
    tool.execute(
        r#"{"path":"all.txt","old_string":"before","new_string":"after","replace_all":true}"#,
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("all.txt")).unwrap(),
        "after after"
    );
    let error = tool
        .execute(r#"{"path":"file.txt","old_string":"after","new_string":"after"}"#)
        .await
        .unwrap_err();
    assert!(error.starts_with("unchanged:"));
}

#[tokio::test]
async fn write_creates_new_file_and_rejects_existing() {
    let root = workspace(&[("old.txt", "existing")]);
    let tool = WriteTool::new(root.path());
    let result = tool
        .execute(r#"{"path":"a/b/file.txt","content":"hello\nworld"}"#)
        .await
        .unwrap();
    let path = root.path().join("a/b/file.txt");
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello\nworld");
    assert_eq!(result, format!("Created file {}", path.display()));

    let error = tool
        .execute(r#"{"path":"old.txt","content":"replacement"}"#)
        .await
        .unwrap_err();
    assert!(error.starts_with("Failed to create file "));
    assert_eq!(
        fs::read_to_string(root.path().join("old.txt")).unwrap(),
        "existing"
    );
}

#[tokio::test]
async fn batch_edits_use_original_and_fail_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample");
    let original = "\u{feff}first\r\nsecond\r\nkeep\nlast";
    fs::write(&path, original).unwrap();
    let registry = crate::tools::ToolRegistry::with_editor(
        root.path(),
        None,
        crate::model::EditorKind::Replace,
    );
    let execute = |edits: serde_json::Value| {
        ToolCall::new(
            "batch",
            "Edit",
            serde_json::json!({"path":"sample", "edits":edits}).to_string(),
        )
    };
    let result = registry
        .execute_call(&execute(serde_json::json!([
            {"old_string":"first", "new_string":"second"},
            {"old_string":"second", "new_string":"third\nfourth"}
        ])))
        .await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "\u{feff}second\r\nthird\r\nfourth\r\nkeep\nlast"
    );
    for (edits, error) in [
        (
            serde_json::json!([{"old_string":"second", "new_string":"ok"},{"old_string":"missing", "new_string":"no"}]),
            "not_found",
        ),
        (
            serde_json::json!([{"old_string":"third\nfourth", "new_string":"x"},{"old_string":"fourth", "new_string":"y"}]),
            "overlap",
        ),
        (
            serde_json::json!([{"old_string":"keep", "new_string":"x", "extra":true}]),
            "Unexpected parameter",
        ),
    ] {
        let before = fs::read(&path).unwrap();
        let result = registry.execute_call(&execute(edits)).await;
        assert!(
            result.is_error && result.content.contains(error),
            "{}",
            result.content
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn stale_version_rejects_edit_and_fresh_version_succeeds() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample");
    fs::write(&path, "value = 1;\n").unwrap();
    let read = ReadTool::new(root.path())
        .execute(r#"{"path":"sample"}"#)
        .await
        .unwrap();
    let version = read
        .lines()
        .next()
        .unwrap()
        .strip_prefix("[version: ")
        .unwrap()
        .strip_suffix(']')
        .unwrap();
    fs::write(&path, "value = 1;\nexternal\n").unwrap();
    let edit = EditTool::new(root.path());
    let arguments = |version: &str| {
        serde_json::json!({"path":"sample", "expected_version":version, "old_string":"value = 1", "new_string":"value = 2"}).to_string()
    };
    assert!(
        edit.execute(&arguments(version))
            .await
            .unwrap_err()
            .starts_with("stale:")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "value = 1;\nexternal\n");
    let version = content_version(&fs::read_to_string(&path).unwrap());
    edit.execute(&arguments(&version)).await.unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "value = 2;\nexternal\n");
}

#[test]
fn repeated_and_overlapping_matches_are_explicit() {
    let edit = |old: &str, all, count| Replacement {
        old_string: old.into(),
        new_string: "z".into(),
        replace_all: Some(all),
        expected_matches: count,
    };
    assert!(
        apply_replacements("x\nx", &[edit("x", false, None)])
            .unwrap_err()
            .contains("lines [1, 2]")
    );
    assert!(
        apply_replacements("aaa", &[edit("aa", false, None)])
            .unwrap_err()
            .starts_with("ambiguous:")
    );
    assert!(
        apply_replacements("aaa", &[edit("aa", true, None)])
            .unwrap_err()
            .starts_with("overlap:")
    );
    assert!(
        apply_replacements("x x", &[edit("x", true, Some(1))])
            .unwrap_err()
            .starts_with("match_count:")
    );
    assert_eq!(
        apply_replacements("x x", &[edit("x", true, Some(2))])
            .unwrap()
            .updated,
        "z z"
    );
}

#[test]
fn staged_write_rechecks_content_and_cleans_up() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample");
    fs::write(&path, "external").unwrap();
    assert!(
        persist_edit(&path, "old", "new")
            .unwrap_err()
            .starts_with("stale:")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "external");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn edit_preserves_symlink_target_and_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    fs::write(&target, "old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o751)).unwrap();
    symlink(&target, root.path().join("link")).unwrap();
    EditTool::new(root.path())
        .execute(r#"{"path":"link","old_string":"old","new_string":"new"}"#)
        .await
        .unwrap();
    assert!(
        fs::symlink_metadata(root.path().join("link"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o751
    );
}
#[test]
fn diff_keeps_distant_edits_compact_and_reports_final_newline() {
    let original: String = (1..=1000).map(|line| format!("line {line}\n")).collect();
    let edits = [
        Replacement {
            old_string: "line 1\n".into(),
            new_string: "first\nextra\n".into(),
            replace_all: None,
            expected_matches: None,
        },
        Replacement {
            old_string: "line 1000\n".into(),
            new_string: "last\n".into(),
            replace_all: None,
            expected_matches: None,
        },
    ];
    let plan = apply_replacements(&original, &edits).unwrap();
    let diff = edit_diff(Path::new("sample"), &original, &plan);
    assert_eq!(
        diff.lines().filter(|line| line.starts_with("@@")).count(),
        2
    );
    assert!(diff.contains("@@ -1,4 +1,5 @@"), "{diff}");
    assert!(diff.contains("@@ -997,4 +998,4 @@"), "{diff}");
    assert!(diff.len() < 500);
    assert!(!diff.contains("line 500"));
    let diff = unified_diff(Path::new("sample"), "value", "value\n", 0, 0);
    assert!(
        diff.contains("-value\n\\ No newline at end of file\n+value"),
        "{diff}"
    );
}

#[test]
fn inserted_lines_follow_local_line_endings() {
    let original = "head\r\ntarget\ntail\r\n";
    let edit = Replacement {
        old_string: "target".into(),
        new_string: "new\nline".into(),
        replace_all: None,
        expected_matches: None,
    };
    assert_eq!(
        apply_replacements(original, &[edit]).unwrap().updated,
        "head\r\nnew\nline\ntail\r\n"
    );
}

#[tokio::test]
async fn concurrent_edits_with_same_version_apply_once() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample");
    fs::write(&path, "original").unwrap();
    let arguments = serde_json::json!({"path":"sample", "expected_version":content_version("original"), "old_string":"original", "new_string":"updated"}).to_string();
    let first = EditTool::new(root.path());
    let second = EditTool::new(root.path());
    let (a, b) = tokio::join!(first.execute(&arguments), second.execute(&arguments));
    assert_ne!(a.is_ok(), b.is_ok());
    let error = if let Err(error) = a {
        error
    } else {
        b.unwrap_err()
    };
    assert!(error.starts_with("stale:"));
    assert_eq!(fs::read_to_string(path).unwrap(), "updated");
}

#[test]
fn edit_locks_tolerate_duplicate_paths() {
    let path = Path::new("/tmp/vva-lock-dup");
    assert_eq!(with_edit_locks(&[path, path], || Ok(1)), Ok(1));
}
