use std::collections::HashSet;
use std::fs;

use super::parse::{Anchor, Op, Payload};
use super::session::*;
use super::snapshot::*;
use super::*;
use crate::model::EditorKind;
use crate::tools::Tool;
use crate::tools::filesystem::{ReadTool, WriteTool};

fn input(body: &str) -> String {
    serde_json::json!({ "input": body }).to_string()
}

#[tokio::test]
async fn unchanged_lines_carry_seen_status_across_edits() {
    let root = std::env::temp_dir().join(format!("vva-hl-carry-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let text: String = (1..=200).map(|n| format!("row {n}\n")).collect();
    fs::write(root.join("sample"), &text).unwrap();
    let session = HashlineSession::new();
    let canonical = fs::canonicalize(root.join("sample")).unwrap();
    let tag = session
        .record(canonical, &text, (10..=10).chain(140..=160))
        .unwrap();
    let edit = HashlineEditTool::new(&root).with_session(session.clone());
    let output = edit
        .execute(&input(&format!(
            "[sample#{tag}]\nPUT 10.=10:\n+a\n+b\n+c\n"
        )))
        .await
        .unwrap();
    let header = output.lines().next().unwrap();
    // 旧 140 行结转为新 142 行，无需 Read 可直接编辑；预览只覆盖 7..=15
    let output2 = edit
        .execute(&input(&format!("{header}\nPUT 142.=142:\n+edited\n")))
        .await
        .unwrap();
    let lines: Vec<String> = fs::read_to_string(root.join("sample"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines[141], "edited");
    assert_eq!(lines[142], "row 141");
    let header2 = output2.lines().next().unwrap();
    let unseen = edit
        .execute(&input(&format!("{header2}\nPUT 20.=20:\n+x\n")))
        .await
        .unwrap_err();
    assert!(unseen.contains("20..=20"), "{unseen}");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn write_registers_created_lines_for_edit() {
    let root = std::env::temp_dir().join(format!("vva-hl-write-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let session = HashlineSession::new();
    let write = WriteTool::new(&root).with_hashline(session.clone());
    let out = write
        .execute(r#"{"path":"sample","content":"a\nb\nc\n"}"#)
        .await
        .unwrap();
    let header = out.lines().nth(1).unwrap();
    assert!(header.starts_with("[sample#"), "{out}");
    let edit = HashlineEditTool::new(&root).with_session(session);
    edit.execute(&input(&format!("{header}\nPUT 2.=2:\n+B\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("sample")).unwrap(),
        "a\nB\nc\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn rebuilt_registry_retains_reads_and_reports_missing_ranges() {
    use crate::tools::{ToolRegistry, WorkspaceToolDeps};
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("sample"), "a\nb\nc\nd\ne\n").unwrap();
    let session = HashlineSession::new();
    let registry = ToolRegistry::with_workspace_tools(
        root.path(),
        None,
        EditorKind::Hashline,
        WorkspaceToolDeps {
            hashline: session.clone(),
            ..Default::default()
        },
    );
    registry
        .get("Read")
        .unwrap()
        .execute(r#"{"path":"sample","offset":1,"limit":1}"#)
        .await
        .unwrap();
    drop(registry);
    let registry = ToolRegistry::with_workspace_tools(
        root.path(),
        None,
        EditorKind::Hashline,
        WorkspaceToolDeps {
            hashline: session,
            ..Default::default()
        },
    );
    let shown = registry
        .get("Read")
        .unwrap()
        .execute(r#"{"path":"sample","offset":3,"limit":1}"#)
        .await
        .unwrap();
    let header = shown.lines().next().unwrap();
    let edit = registry.get("Edit").unwrap();
    let error = edit
        .execute(&input(&format!("{header}\nPUT 1.=5:\n+x\n")))
        .await
        .unwrap_err();
    assert!(error.contains("op 1"), "{error}");
    assert!(error.contains("2..=2, 4..=5"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
    assert_eq!(
        fs::read_to_string(root.path().join("sample")).unwrap(),
        "a\nb\nc\nd\ne\n"
    );
    edit.execute(&input(&format!("{header}\nPUT 1.=1:\n+A\nPUT 3.=3:\n+C\n")))
        .await
        .unwrap();
}

#[tokio::test]
async fn distant_hunks_have_local_diff_and_reusable_shifted_preview() {
    let root = tempfile::tempdir().unwrap();
    let text: String = (1..=160).map(|n| format!("row {n}\n")).collect();
    let path = root.path().join("sample");
    fs::write(&path, &text).unwrap();
    let session = HashlineSession::new();
    let tag = session
        .record(fs::canonicalize(&path).unwrap(), &text, 1..=160)
        .unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session.clone());
    let output = edit
        .execute(&input(&format!(
            "[sample#{tag}]\nPUT 12.=12:\n+first\n+extra\nPUT 140.=140:\n+second\n"
        )))
        .await
        .unwrap();
    assert_eq!(output.matches("@@ -").count(), 2, "{output}");
    assert!(!output.contains("row 80"), "{output}");
    assert!(output.contains("141:second"), "{output}");
    let current = fs::read_to_string(&path).unwrap();
    let snap = session
        .lookup(&fs::canonicalize(&path).unwrap(), &file_tag(&current))
        .unwrap();
    assert!(snap.seen.contains(&141));
    // 变更 span 之外的行结转：旧 79 行平移为新 80 行
    assert!(snap.seen.contains(&80));
    let header = output.lines().next().unwrap();
    edit.execute(&input(&format!("{header}\nPUT 141.=141:\n+final\n")))
        .await
        .unwrap();
}

#[test]
fn previews_cover_deletions_and_elide_large_replacements() {
    let text: String = (1..=100).map(|n| format!("row {n}\n")).collect();
    let preview = numbered_preview(&text, &[(10..90, 10..90)]);
    assert_eq!(preview.len(), 16);
    assert!(!preview.iter().any(|(n, _)| *n == 50));
    let preview = numbered_preview(&text, &[(100..110, 100..100)]);
    assert_eq!(
        preview.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![98, 99, 100]
    );
    let session = HashlineSession::new();
    let report = report_update(
        &session,
        "sample",
        PathBuf::from("sample"),
        "last",
        "",
        vec![(0..1, 0..1)],
        &HashSet::new(),
    )
    .unwrap();
    assert!(report.contains("-last"));
}

#[test]
fn parse_cut_shorthand_is_single_line_range() {
    let sections = parse_patch("[a.rs#ABCD]\nCUT 5\nCUT 2 @buf\nCUT 3.=7\nCUT 4*\n").unwrap();
    assert_eq!(
        sections[0].ops,
        vec![
            Op {
                anchor: Anchor::Range { start: 5, end: 5 },
                payload: Payload::Cut(None),
            },
            Op {
                anchor: Anchor::Range { start: 2, end: 2 },
                payload: Payload::Cut(Some("buf".into())),
            },
            Op {
                anchor: Anchor::Range { start: 3, end: 7 },
                payload: Payload::Cut(None),
            },
            Op {
                anchor: Anchor::Block { line: 4 },
                payload: Payload::Cut(None),
            },
        ]
    );
}

#[test]
fn parse_cut_shorthand_named_register_paste() {
    let sections = parse_patch("[a.rs#ABCD]\nCUT 5 @head\nPUT 1.=2 @head\n").unwrap();
    assert_eq!(
        sections[0].ops,
        vec![
            Op {
                anchor: Anchor::Range { start: 5, end: 5 },
                payload: Payload::Cut(Some("head".into())),
            },
            Op {
                anchor: Anchor::Range { start: 1, end: 2 },
                payload: Payload::Register(Some("head".into())),
            },
        ]
    );
}

#[test]
fn parse_rejects_malformed_put_and_cut_syntax() {
    for input in [
        "[a.rs#ABCD]\nPUT 5:\n+x\n",
        "[a.rs#ABCD]\nPUT 5\n",
        "[a.rs#ABCD]\nPUT 5 @head\n",
        "[a.rs#ABCD]\nPUT 1-2:\n+x\n",
        "[a.rs#ABCD]\nPUT 5.=3:\n+x\n",
        "[a.rs#ABCD]\nPUT 1.:\n+x\n",
        "[a.rs#ABCD]\nCUT 5:\n",
        "[a.rs#ABCD]\nCUT 5:\n+x\n",
        "[a.rs#ABCD]\nCUT 5.=5:\n",
        "[a.rs#ABCD]\nCUT <5\n",
        "[a.rs#ABCD]\nCUT 5foo\n",
        "[a.rs#ABCD]\nCUT 5.=\n",
    ] {
        assert!(parse_patch(input).is_err(), "{input}");
    }
}

#[tokio::test]
async fn read_then_put_replace_and_insert() {
    let root = std::env::temp_dir().join(format!("vva-hl-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("sample"), "alpha\nbeta\ngamma\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(&root)
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"sample"}"#).await.unwrap();
    assert!(shown.starts_with("[sample#"));
    assert!(shown.contains("1:alpha"));
    let tag = shown
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("[sample#")
        .trim_end_matches(']');
    let edit = HashlineEditTool::new(&root).with_session(session);
    edit.execute(&input(&format!(
        "[sample#{tag}]\nPUT 1.=1:\n+ALPHA\nPUT >2:\n+mid\nPUT 3.=3:\n+GAMMA\n+TAIL\n"
    )))
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("sample")).unwrap(),
        "ALPHA\nbeta\nmid\nGAMMA\nTAIL\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn put_star_replaces_brace_block() {
    let root = std::env::temp_dir().join(format!("vva-hl-block-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("sample"), "fn greet() {\n    hi()\n}\ncall();\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(&root)
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"sample"}"#).await.unwrap();
    let tag = shown
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("[sample#")
        .trim_end_matches(']');
    let edit = HashlineEditTool::new(&root).with_session(session);
    edit.execute(&input(&format!(
        "[sample#{tag}]\nPUT 1*:\n+fn greet() {{\n+    hello()\n+}}\n"
    )))
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("sample")).unwrap(),
        "fn greet() {\n    hello()\n}\ncall();\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn cut_paste_named_register() {
    let root = std::env::temp_dir().join(format!("vva-hl-clip-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a"), "one\ntwo\nthree\n").unwrap();
    fs::write(root.join("b"), "keep\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(&root)
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let a = read.execute(r#"{"path":"a"}"#).await.unwrap();
    let b = read.execute(r#"{"path":"b"}"#).await.unwrap();
    let tag_a = a
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("[a#")
        .trim_end_matches(']');
    let tag_b = b
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("[b#")
        .trim_end_matches(']');
    let edit = HashlineEditTool::new(&root).with_session(session);
    edit.execute(&input(&format!(
        "[a#{tag_a}]\nCUT 1.=2 @head\n[b#{tag_b}]\nPUT <1 @head\n"
    )))
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "three\n");
    assert_eq!(
        fs::read_to_string(root.join("b")).unwrap(),
        "one\ntwo\nkeep\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn indentation_blocks_preserve_siblings_and_separators() {
    let lines = [
        "class One:",
        "",
        "    def method(self):",
        "        # keep this comment",
        "        if True:",
        "            return call()",
        "",
        "    def sibling(self):",
        "        pass",
        "",
        "class Two(Base):",
        "\tpass",
        "",
        "after()",
    ];
    assert_eq!(block::resolve_block(&lines, 1, "a.py"), Ok((1, 9)));
    assert_eq!(block::resolve_block(&lines, 3, "a.py"), Ok((3, 6)));
    assert_eq!(block::resolve_block(&lines, 5, "a.py"), Ok((5, 6)));
    assert_eq!(block::resolve_block(&lines, 11, "a.py"), Ok((11, 12)));
    assert!(block::resolve_block(&["value = 1", "other()"], 1, "a.py").is_err());
    assert!(block::resolve_block(&["def one(): pass", "def two(): pass"], 1, "a.py").is_err());
}

#[tokio::test]
async fn python_blocks_support_replace_cut_and_insert_after() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("sample.py"),
        "def one():\n    # body comment\n    return 1\n\ndef two():\n    return 2\n\nclass Three(Base):\n    pass\n\nafter()\n",
    )
    .unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"sample.py"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    edit.execute(&input(&format!(
        "{header}\nPUT 1*:\n+def one():\n+    return 10\nCUT 5* @two\nPUT >8*:\n+inserted()\nPUT >$ @two\n"
    )))
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("sample.py")).unwrap(),
        "def one():\n    return 10\n\n\nclass Three(Base):\n    pass\ninserted()\n\nafter()\ndef two():\n    return 2\n"
    );
}

#[test]
fn delimiter_scan_skips_strings_and_comments() {
    let reported = ["fn f() {", "    let s = \"}\";", "    let t = 1;", "}"];
    assert_eq!(block::resolve_block(&reported, 1, "a.rs"), Ok((1, 4)));

    let line_comment = ["fn f() {", "    // }", "    x();", "}"];
    assert_eq!(block::resolve_block(&line_comment, 1, "a.rs"), Ok((1, 4)));

    let block_comment = ["fn f() {", "    /* } */", "    x();", "}"];
    assert_eq!(block::resolve_block(&block_comment, 1, "a.rs"), Ok((1, 4)));

    let same_line_single_quote = [
        "def f():",
        "    s = '}'",
        "    x = 1",
        "",
        "def g():",
        "    pass",
    ];
    assert_eq!(
        block::resolve_block(&same_line_single_quote, 1, "a.py"),
        Ok((1, 3))
    );

    let lifetime = ["fn f<'a>(x: &'a str) {", "    x;", "}"];
    assert_eq!(block::resolve_block(&lifetime, 1, "a.rs"), Ok((1, 3)));

    let attribute = ["#[derive(Debug)]", "struct S {", "    a: u8,", "}"];
    assert_eq!(block::resolve_block(&attribute, 2, "a.rs"), Ok((2, 4)));

    let docstring = [
        "def f():",
        "    \"\"\"",
        "    }",
        "    \"\"\"",
        "    return 1",
        "",
        "def g():",
        "    pass",
    ];
    assert_eq!(block::resolve_block(&docstring, 1, "a.py"), Ok((1, 5)));
}

#[tokio::test]
async fn multi_section_mv_to_missing_directory_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("p1.rs"), "alpha\nbeta\n").unwrap();
    fs::write(root.path().join("p2.rs"), "x\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let p1 = read.execute(r#"{"path":"p1.rs"}"#).await.unwrap();
    let p2 = read.execute(r#"{"path":"p2.rs"}"#).await.unwrap();
    fn header(output: &str) -> &str {
        output.lines().next().unwrap()
    }
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!(
            "{}\nPUT 1.=1:\n+ALPHA\n{}\nMV missing_dir/renamed.rs\n",
            header(&p1),
            header(&p2)
        )))
        .await
        .unwrap_err();
    assert!(error.contains("does not exist"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
    assert_eq!(
        fs::read_to_string(root.path().join("p1.rs")).unwrap(),
        "alpha\nbeta\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("p2.rs")).unwrap(),
        "x\n"
    );
    assert!(!root.path().join("missing_dir").exists());
}

#[tokio::test]
async fn mv_bare_name_renames_in_source_directory() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    fs::write(root.path().join("sub/orig.rs"), "one\ntwo\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"sub/orig.rs"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let report = edit
        .execute(&input(&format!(
            "{header}\nPUT 2.=2:\n+TWO\nMV renamed.rs\n"
        )))
        .await
        .unwrap();
    assert!(report.starts_with("[sub/renamed.rs#"), "{report}");
    assert_eq!(
        fs::read_to_string(root.path().join("sub/renamed.rs")).unwrap(),
        "one\nTWO\n"
    );
    assert!(!root.path().join("sub/orig.rs").exists());
    assert!(!root.path().join("renamed.rs").exists());
}

#[tokio::test]
async fn mv_numeric_tokens_are_rejected_as_line_move_misread() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("sample"), "a\nb\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"sample"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!("{header}\nMV 1 5\n")))
        .await
        .unwrap_err();
    assert!(error.contains("line numbers"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
    assert_eq!(
        fs::read_to_string(root.path().join("sample")).unwrap(),
        "a\nb\n"
    );
}

#[tokio::test]
async fn partial_line_continuation_unlocks_edit() {
    let root = tempfile::tempdir().unwrap();
    let long_line = "x".repeat(40 * 1024);
    fs::write(root.path().join("big.txt"), format!("short\n{long_line}\n")).unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let first = read
        .execute(r#"{"path":"big.txt","offset":2,"limit":1}"#)
        .await
        .unwrap();
    let marker = first.lines().last().unwrap();
    assert!(marker.contains("[Partial line;"), "{marker}");
    let byte_offset = marker
        .split("byte_offset=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap();
    let second = read
        .execute(&format!(
            r#"{{"path":"big.txt","offset":2,"limit":1,"byte_offset":{byte_offset}}}"#
        ))
        .await
        .unwrap();
    assert!(second.contains("2:"), "{second}");
    let header = first.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    edit.execute(&input(&format!("{header}\nPUT 2.=2:\n+replaced\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("big.txt")).unwrap(),
        "short\nreplaced\n"
    );
}

#[tokio::test]
async fn cold_byte_offset_read_does_not_mark_line_seen() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("lines.txt"), "short\n0123456789abcdefgh\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read
        .execute(r#"{"path":"lines.txt","offset":2,"limit":1,"byte_offset":10}"#)
        .await
        .unwrap();
    assert!(shown.contains("2:abcdefgh"), "{shown}");
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!("{header}\nPUT 2.=2:\n+no\n")))
        .await
        .unwrap_err();
    assert!(error.contains("unseen"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
}

#[tokio::test]
async fn out_of_range_lines_report_existence_not_unseen() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("small.txt"), "a\nb\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"small.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!("{header}\nPUT 99.=100:\n+z\n")))
        .await
        .unwrap_err();
    assert!(
        error.contains("line 100 does not exist (file has 2 lines)"),
        "{error}"
    );
    let error = edit
        .execute(&input(&format!("{header}\nPUT >99:\n+z\n")))
        .await
        .unwrap_err();
    assert!(error.contains("line 99 does not exist"), "{error}");
}

#[tokio::test]
async fn paste_inside_cut_range_gets_guidance_and_outside_anchor_works() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("reorder.txt"),
        "h1\n{\n    a\n    b\n}\nh2\n",
    )
    .unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"reorder.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!("{header}\nCUT 2* @blk\nPUT >4 @blk\n")))
        .await
        .unwrap_err();
    assert!(
        error.contains("overlap") && error.contains("inside the lines removed"),
        "{error}"
    );
    assert!(error.contains(">5"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
    edit.execute(&input(&format!("{header}\nCUT 2* @blk\nPUT >6 @blk\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("reorder.txt")).unwrap(),
        "h1\nh2\n{\n    a\n    b\n}\n"
    );
}

#[tokio::test]
async fn grep_clips_huge_line_and_partial_read_unlocks_edit() {
    use crate::tools::filesystem::GrepTool;

    let root = tempfile::tempdir().unwrap();
    let long = "y".repeat(40 * 1024);
    fs::write(
        root.path().join("huge.txt"),
        format!("head\n{long}\ntail\n"),
    )
    .unwrap();
    let session = HashlineSession::new();
    let grep = GrepTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let out = grep
        .execute(r#"{"pattern":"^y+$","path":"huge.txt"}"#)
        .await
        .unwrap();
    assert!(
        out.len() <= crate::tools::MAX_INLINE_OUTPUT_BYTES,
        "{}",
        out.len()
    );
    assert!(
        out.contains("[Partial line; continue with Read offset=2, byte_offset="),
        "{out}"
    );
    let header = out.lines().next().unwrap();
    let byte_offset = out
        .split("byte_offset=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap();

    let edit = HashlineEditTool::new(root.path()).with_session(session.clone());
    let error = edit
        .execute(&input(&format!("{header}\nPUT 2.=2:\n+replaced\n")))
        .await
        .unwrap_err();
    assert!(error.contains("unseen"), "{error}");

    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    read.execute(&format!(
        r#"{{"path":"huge.txt","offset":2,"limit":1,"byte_offset":{byte_offset}}}"#
    ))
    .await
    .unwrap();
    edit.execute(&input(&format!("{header}\nPUT 2.=2:\n+replaced\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("huge.txt")).unwrap(),
        "head\nreplaced\ntail\n"
    );
}

#[tokio::test]
async fn grep_registers_fully_shown_lines() {
    use crate::tools::filesystem::GrepTool;

    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("find.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let session = HashlineSession::new();
    let grep = GrepTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let out = grep
        .execute(r#"{"pattern":"beta","path":"find.txt"}"#)
        .await
        .unwrap();
    assert!(out.contains("2:beta"), "{out}");
    let header = out.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    edit.execute(&input(&format!("{header}\nPUT 2.=2:\n+BETA\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("find.txt")).unwrap(),
        "alpha\nBETA\ngamma\n"
    );
}

#[test]
fn body_row_after_register_paste_reports_paste_semantics() {
    let error = parse_patch("[a.rs#ABCD]\nCUT 2 @blk\nPUT >1 @blk\n+oops\n").unwrap_err();
    assert!(error.contains("register paste"), "{error}");
    let error = parse_patch("[a.rs#ABCD]\nCUT 2 @blk\n+oops\n").unwrap_err();
    assert!(error.contains("takes no body rows"), "{error}");
}

#[tokio::test]
async fn preview_gap_markers_match_enforcement() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("six.txt"),
        "one\ntwo\nthree\nfour\nfive\nsix\n",
    )
    .unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"six.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap().to_string();
    let edit = HashlineEditTool::new(root.path()).with_session(session.clone());
    let report = edit
        .execute(&input(&format!("{header}\nPUT 1.=1:\n+ONE\n")))
        .await
        .unwrap();
    assert!(
        report.contains("[Lines 5-6 unchanged; shown earlier, editable without Read]"),
        "{report}"
    );
    assert!(!report.contains("Read before editing"), "{report}");
    let new_header = report.lines().next().unwrap();
    edit.execute(&input(&format!("{new_header}\nPUT 6.=6:\n+SIX\n")))
        .await
        .unwrap();

    let root2 = tempfile::tempdir().unwrap();
    fs::write(
        root2.path().join("six.txt"),
        "one\ntwo\nthree\nfour\nfive\nsix\n",
    )
    .unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root2.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read
        .execute(r#"{"path":"six.txt","offset":1,"limit":2}"#)
        .await
        .unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root2.path()).with_session(session);
    let report = edit
        .execute(&input(&format!("{header}\nPUT 2.=2:\n+TWO\n")))
        .await
        .unwrap();
    assert!(
        report.contains("not displayed this session; Read before editing]"),
        "{report}"
    );
    let new_header = report.lines().next().unwrap();
    let error = edit
        .execute(&input(&format!("{new_header}\nPUT 6.=6:\n+SIX\n")))
        .await
        .unwrap_err();
    assert!(error.contains("unseen"), "{error}");
}

#[test]
fn malformed_headers_report_tag_format() {
    let error = parse_patch("[a.rs#????]\nPUT 1.=1:\n+x\n").unwrap_err();
    assert!(error.contains("not hexadecimal"), "{error}");
    let error = parse_patch("[a.rs#AB]\nPUT 1.=1:\n+x\n").unwrap_err();
    assert!(error.contains("4 hex characters"), "{error}");
}

#[test]
fn member_line_block_error_names_enclosing_opener() {
    let yaml = ["main:", "    call a", "    call b"];
    let error = block::resolve_block(&yaml, 2, "t20").unwrap_err();
    assert!(
        error.contains("inside the block opening at line 1"),
        "{error}"
    );
    assert!(error.contains("PUT 1*"), "{error}");

    let brace = ["fn a() {", "    x();", "}"];
    let error = block::resolve_block(&brace, 2, "a.rs").unwrap_err();
    assert!(
        error.contains("inside the block opening at line 1"),
        "{error}"
    );

    let standalone = ["let a = (1);", "let b = (2);"];
    let error = block::resolve_block(&standalone, 1, "a.rs").unwrap_err();
    assert!(error.contains("single-line node"), "{error}");
}

#[tokio::test]
async fn markdown_section_replace_preserves_separator_blank() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("doc.md"),
        "# T\n\n## A\nold body\n\n## B\nkeep\n",
    )
    .unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"doc.md"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    edit.execute(&input(&format!("{header}\nPUT 3*:\n+## A\n+new body\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("doc.md")).unwrap(),
        "# T\n\n## A\nnew body\n\n## B\nkeep\n"
    );
}

#[tokio::test]
async fn append_to_empty_file_adds_trailing_newline() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("empty.txt"), "").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"empty.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    edit.execute(&input(&format!("{header}\nPUT >$:\n+first\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("empty.txt")).unwrap(),
        "first\n"
    );
}

#[tokio::test]
async fn docstring_at_column_zero_stays_inside_block() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("t.py"),
        "def a():\n    s = \"\"\"\ncolumn zero doc\n\"\"\"\n    return 1\n",
    )
    .unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"t.py"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    edit.execute(&input(&format!(
        "{header}\nPUT 1*:\n+def a():\n+    return 2\n"
    )))
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("t.py")).unwrap(),
        "def a():\n    return 2\n"
    );
}

#[tokio::test]
async fn bom_survives_line_one_replacement() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("bom.txt"), "\u{feff}first\nsecond\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"bom.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    edit.execute(&input(&format!("{header}\nPUT 1.=1:\n+FIRST\n")))
        .await
        .unwrap();
    let disk = fs::read(root.path().join("bom.txt")).unwrap();
    assert_eq!(&disk[..3], [0xef, 0xbb, 0xbf]);
    assert_eq!(
        String::from_utf8_lossy(&disk[3..]).to_string(),
        "FIRST\nsecond\n"
    );
}

#[tokio::test]
async fn mv_angle_bracket_dest_rejected_but_wrapped_placeholder_unwrapped() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("src.txt"), "x\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"src.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!("{header}\nMV <renamed.txt\n")))
        .await
        .unwrap_err();
    assert!(error.contains("must not start with '<'"), "{error}");
    assert!(error.contains("MV renamed.txt"), "{error}");
    assert!(!root.path().join("<renamed.txt").exists());
}

#[tokio::test]
async fn stale_tag_printed_in_error_is_reusable() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("st.txt"), "a\nb\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"st.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    fs::write(root.path().join("st.txt"), "a\nb\nEXTERNAL\n").unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!("{header}\nPUT 1.=1:\n+A\n")))
        .await
        .unwrap_err();
    assert!(error.contains("stale"), "{error}");
    let tag = error.split("(now [st.txt#").nth(1).unwrap()[..4].to_string();
    let error = edit
        .execute(&input(&format!("[st.txt#{tag}]\nPUT 1.=1:\n+A\n")))
        .await
        .unwrap_err();
    assert!(error.contains("unseen"), "{error}");
    assert!(!error.contains("not registered"), "{error}");
}

#[tokio::test]
async fn empty_register_error_names_the_register() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("r.txt"), "a\nb\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(root.path())
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read.execute(r#"{"path":"r.txt"}"#).await.unwrap();
    let header = shown.lines().next().unwrap();
    let edit = HashlineEditTool::new(root.path()).with_session(session);
    let error = edit
        .execute(&input(&format!("{header}\nPUT >2 @cap\n")))
        .await
        .unwrap_err();
    assert!(error.contains("register @cap is empty"), "{error}");
}
