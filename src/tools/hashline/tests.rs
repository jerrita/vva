use std::collections::HashSet;
use std::fs;

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
    use crate::tools::ToolRegistry;
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("sample"), "a\nb\nc\nd\ne\n").unwrap();
    let session = HashlineSession::new();
    let registry =
        ToolRegistry::with_editor_session(root.path(), None, EditorKind::Hashline, session.clone());
    registry
        .get("Read")
        .unwrap()
        .execute(r#"{"path":"sample","offset":1,"limit":1}"#)
        .await
        .unwrap();
    drop(registry);
    let registry =
        ToolRegistry::with_editor_session(root.path(), None, EditorKind::Hashline, session);
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
fn parse_put_cut_and_header() {
    let sections =
        parse_patch("[greet.py#A1B2]\nPUT 1.=3:\n+def greet(name):\n+    print(name)\nCUT 4.=4\n")
            .unwrap();
    assert_eq!(sections[0].path, "greet.py");
    assert_eq!(sections[0].tag, "A1B2");
    assert_eq!(sections[0].ops.len(), 2);
}

#[test]
fn parse_rejects_range_without_dot_equals() {
    let err = parse_patch("[a.rs#ABCD]\nPUT 1-2:\n+x\n").unwrap_err();
    assert!(err.contains("N.=M"), "{err}");
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
async fn unseen_and_stale_fail() {
    let root = std::env::temp_dir().join(format!("vva-hl-fail-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("sample"), "a\nb\nc\n").unwrap();
    let session = HashlineSession::new();
    let read = ReadTool::new(&root)
        .with_editor(EditorKind::Hashline)
        .with_hashline(session.clone());
    let shown = read
        .execute(r#"{"path":"sample","offset":1,"limit":1}"#)
        .await
        .unwrap();
    let tag = shown
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("[sample#")
        .trim_end_matches(']');
    let edit = HashlineEditTool::new(&root).with_session(session.clone());
    let unseen = edit
        .execute(&input(&format!("[sample#{tag}]\nPUT 3.=3:\n+C\n")))
        .await
        .unwrap_err();
    assert!(unseen.contains("unseen"), "{unseen}");
    fs::write(root.join("sample"), "a\nb\nc\nchanged\n").unwrap();
    let stale = edit
        .execute(&input(&format!("[sample#{tag}]\nPUT 1.=1:\n+A\n")))
        .await
        .unwrap_err();
    assert!(stale.contains("stale"), "{stale}");
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
