use std::collections::HashSet;
use std::fs;

use super::parse::{Anchor, Op, Payload};
use super::session::*;
use super::snapshot::*;
use super::*;
use crate::model::EditorKind;
use crate::tools::Tool;
use crate::tools::filesystem::{GrepTool, ReadTool, WriteTool};
use std::sync::Arc;

fn input(body: &str) -> String {
    serde_json::json!({ "input": body }).to_string()
}

#[tokio::test]
async fn unchanged_lines_carry_seen_status_across_edits() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let text: String = (1..=200).map(|n| format!("row {n}\n")).collect();
    fs::write(root.join("sample"), &text).unwrap();
    let session = HashlineSession::new();
    let canonical = fs::canonicalize(root.join("sample")).unwrap();
    let tag = session
        .record(canonical, &text, (10..=10).chain(140..=160))
        .unwrap();
    let edit = HashlineEditTool::new(root).with_session(session.clone());
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
}

#[tokio::test]
async fn write_registers_created_lines_for_edit() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let session = HashlineSession::new();
    let write = WriteTool::new(root).with_hashline(session.clone());
    let out = write
        .execute(r#"{"path":"sample","content":"a\nb\nc\n"}"#)
        .await
        .unwrap();
    let header = out.lines().nth(1).unwrap();
    assert!(header.starts_with("[sample#"), "{out}");
    let edit = HashlineEditTool::new(root).with_session(session);
    edit.execute(&input(&format!("{header}\nPUT 2.=2:\n+B\n")))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("sample")).unwrap(),
        "a\nB\nc\n"
    );
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
fn parse_cut_shorthand_ranges_and_registers() {
    let cut = |start, end, register: Option<&str>| Op {
        anchor: Anchor::Range { start, end },
        payload: Payload::Cut(register.map(Into::into)),
    };
    let sections = parse_patch("[a.rs#ABCD]\nCUT 5\nCUT 2 @buf\nCUT 3.=7\nCUT 4*\n").unwrap();
    assert_eq!(
        sections[0].ops,
        vec![
            cut(5, 5, None),
            cut(2, 2, Some("buf")),
            cut(3, 7, None),
            Op {
                anchor: Anchor::Block { line: 4 },
                payload: Payload::Cut(None),
            },
        ]
    );
    let sections = parse_patch("[a.rs#ABCD]\nCUT 5 @head\nPUT 1.=2 @head\n").unwrap();
    assert_eq!(
        sections[0].ops,
        vec![
            cut(5, 5, Some("head")),
            Op {
                anchor: Anchor::Range { start: 1, end: 2 },
                payload: Payload::Register(Some("head".into())),
            },
        ]
    );
}

#[test]
fn parse_rejects_malformed_syntax_with_guidance() {
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
    for (input, fragment) in [
        (
            "[a.rs#ABCD]\nCUT 2 @blk\nPUT >1 @blk\n+oops\n",
            "register paste",
        ),
        ("[a.rs#ABCD]\nCUT 2 @blk\n+oops\n", "takes no body rows"),
        ("[a.rs#????]\nPUT 1.=1:\n+x\n", "not hexadecimal"),
        ("[a.rs#AB]\nPUT 1.=1:\n+x\n", "4 hex characters"),
    ] {
        let error = parse_patch(input).unwrap_err();
        assert!(error.contains(fragment), "{error}");
    }
}

/// 临时工作区 + 共享 hashline 会话：Read / Edit 走同一快照登记。
struct Fx {
    dir: tempfile::TempDir,
    session: Arc<HashlineSession>,
}

impl Fx {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        Self {
            dir,
            session: HashlineSession::new(),
        }
    }

    fn text(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap()
    }

    fn read_tool(&self) -> ReadTool {
        ReadTool::new(self.dir.path())
            .with_editor(EditorKind::Hashline)
            .with_hashline(self.session.clone())
    }

    fn grep_tool(&self) -> GrepTool {
        GrepTool::new(self.dir.path())
            .with_editor(EditorKind::Hashline)
            .with_hashline(self.session.clone())
    }

    fn edit_tool(&self) -> HashlineEditTool {
        HashlineEditTool::new(self.dir.path()).with_session(self.session.clone())
    }

    async fn read(&self, args: &str) -> String {
        self.read_tool().execute(args).await.unwrap()
    }

    /// Read 整个文件并返回 `[PATH#TAG]` 头。
    async fn header(&self, path: &str) -> String {
        let shown = self.read(&format!(r#"{{"path":"{path}"}}"#)).await;
        shown.lines().next().unwrap().to_string()
    }

    async fn apply(&self, patch: &str) -> Result<String, String> {
        self.edit_tool().execute(&input(patch)).await
    }

    /// 读取 `path` 取得头后应用 `ops`。
    async fn patch(&self, path: &str, ops: &str) -> Result<String, String> {
        let header = self.header(path).await;
        self.apply(&format!("{header}\n{ops}")).await
    }
}

#[tokio::test]
async fn read_then_put_replace_insert_and_blocks() {
    let fx = Fx::new(&[
        ("sample", "alpha\nbeta\ngamma\n"),
        ("blk", "fn greet() {\n    hi()\n}\ncall();\n"),
        ("doc.md", "# T\n\n## A\nold body\n\n## B\nkeep\n"),
        (
            "t.py",
            "def a():\n    s = \"\"\"\ncolumn zero doc\n\"\"\"\n    return 1\n",
        ),
        (
            "sample.py",
            "def one():\n    # body comment\n    return 1\n\ndef two():\n    return 2\n\nclass Three(Base):\n    pass\n\nafter()\n",
        ),
    ]);
    let shown = fx.read(r#"{"path":"sample"}"#).await;
    assert!(shown.starts_with("[sample#") && shown.contains("1:alpha"));
    fx.patch(
        "sample",
        "PUT 1.=1:\n+ALPHA\nPUT >2:\n+mid\nPUT 3.=3:\n+GAMMA\n+TAIL\n",
    )
    .await
    .unwrap();
    assert_eq!(fx.text("sample"), "ALPHA\nbeta\nmid\nGAMMA\nTAIL\n");

    fx.patch("blk", "PUT 1*:\n+fn greet() {\n+    hello()\n+}\n")
        .await
        .unwrap();
    assert_eq!(fx.text("blk"), "fn greet() {\n    hello()\n}\ncall();\n");

    fx.patch("doc.md", "PUT 3*:\n+## A\n+new body\n")
        .await
        .unwrap();
    assert_eq!(fx.text("doc.md"), "# T\n\n## A\nnew body\n\n## B\nkeep\n");

    fx.patch("t.py", "PUT 1*:\n+def a():\n+    return 2\n")
        .await
        .unwrap();
    assert_eq!(fx.text("t.py"), "def a():\n    return 2\n");

    fx.patch(
        "sample.py",
        "PUT 1*:\n+def one():\n+    return 10\nCUT 5* @two\nPUT >8*:\n+inserted()\nPUT >$ @two\n",
    )
    .await
    .unwrap();
    assert_eq!(
        fx.text("sample.py"),
        "def one():\n    return 10\n\n\nclass Three(Base):\n    pass\ninserted()\n\nafter()\ndef two():\n    return 2\n"
    );
}

#[tokio::test]
async fn cut_paste_named_register_across_files() {
    let fx = Fx::new(&[("a", "one\ntwo\nthree\n"), ("b", "keep\n")]);
    let (a, b) = (fx.header("a").await, fx.header("b").await);
    fx.apply(&format!("{a}\nCUT 1.=2 @head\n{b}\nPUT <1 @head\n"))
        .await
        .unwrap();
    assert_eq!(fx.text("a"), "three\n");
    assert_eq!(fx.text("b"), "one\ntwo\nkeep\n");
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

#[test]
fn delimiter_scan_skips_strings_and_comments() {
    type Case<'a> = (&'a [&'a str], usize, &'a str, (usize, usize));
    let cases: [Case; 7] = [
        (
            &["fn f() {", "    let s = \"}\";", "    let t = 1;", "}"],
            1,
            "a.rs",
            (1, 4),
        ),
        (
            &["fn f() {", "    // }", "    x();", "}"],
            1,
            "a.rs",
            (1, 4),
        ),
        (
            &["fn f() {", "    /* } */", "    x();", "}"],
            1,
            "a.rs",
            (1, 4),
        ),
        (
            &[
                "def f():",
                "    s = '}'",
                "    x = 1",
                "",
                "def g():",
                "    pass",
            ],
            1,
            "a.py",
            (1, 3),
        ),
        (
            &["fn f<'a>(x: &'a str) {", "    x;", "}"],
            1,
            "a.rs",
            (1, 3),
        ),
        (
            &["#[derive(Debug)]", "struct S {", "    a: u8,", "}"],
            2,
            "a.rs",
            (2, 4),
        ),
        (
            &[
                "def f():",
                "    \"\"\"",
                "    }",
                "    \"\"\"",
                "    return 1",
                "",
                "def g():",
                "    pass",
            ],
            1,
            "a.py",
            (1, 5),
        ),
    ];
    for (lines, line, path, want) in cases {
        assert_eq!(
            block::resolve_block(lines, line as u32, path),
            Ok((want.0 as u32, want.1 as u32)),
            "{lines:?}"
        );
    }
}

#[test]
fn block_errors_name_enclosing_opener() {
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
async fn mv_variants() {
    // 多 section 中 MV 目标目录缺失：预检失败，任何文件都不变
    let fx = Fx::new(&[
        ("p1.rs", "alpha\nbeta\n"),
        ("p2.rs", "x\n"),
        ("sub/orig.rs", "one\ntwo\n"),
        ("sample", "a\nb\n"),
    ]);
    let (h1, h2) = (fx.header("p1.rs").await, fx.header("p2.rs").await);
    let error = fx
        .apply(&format!(
            "{h1}\nPUT 1.=1:\n+ALPHA\n{h2}\nMV missing_dir/renamed.rs\n"
        ))
        .await
        .unwrap_err();
    assert!(error.contains("does not exist"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
    assert_eq!(fx.text("p1.rs"), "alpha\nbeta\n");
    assert_eq!(fx.text("p2.rs"), "x\n");
    assert!(!fx.dir.path().join("missing_dir").exists());

    // 裸文件名在源文件所在目录内改名
    let report = fx
        .patch("sub/orig.rs", "PUT 2.=2:\n+TWO\nMV renamed.rs\n")
        .await
        .unwrap();
    assert!(report.starts_with("[sub/renamed.rs#"), "{report}");
    assert_eq!(fx.text("sub/renamed.rs"), "one\nTWO\n");
    assert!(!fx.dir.path().join("sub/orig.rs").exists());
    assert!(!fx.dir.path().join("renamed.rs").exists());

    // 纯数字被视为行号误用；<dest> 包装被拒绝并提示
    let error = fx.patch("sample", "MV 1 5\n").await.unwrap_err();
    assert!(error.contains("line numbers"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
    assert_eq!(fx.text("sample"), "a\nb\n");
    let error = fx.patch("sample", "MV <renamed.txt\n").await.unwrap_err();
    assert!(error.contains("must not start with '<'"), "{error}");
    assert!(error.contains("MV renamed.txt"), "{error}");
    assert!(!fx.dir.path().join("<renamed.txt").exists());
}

#[tokio::test]
async fn partial_line_continuation_unlocks_edit() {
    let long_line = "x".repeat(40 * 1024);
    let fx = Fx::new(&[("big.txt", &format!("short\n{long_line}\n"))]);
    let first = fx.read(r#"{"path":"big.txt","offset":2,"limit":1}"#).await;
    let marker = first.lines().last().unwrap();
    assert!(marker.contains("[Partial line;"), "{marker}");
    let byte_offset = marker
        .split("byte_offset=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap();
    let second = fx
        .read(&format!(
            r#"{{"path":"big.txt","offset":2,"limit":1,"byte_offset":{byte_offset}}}"#
        ))
        .await;
    assert!(second.contains("2:"), "{second}");
    let header = first.lines().next().unwrap();
    fx.apply(&format!("{header}\nPUT 2.=2:\n+replaced\n"))
        .await
        .unwrap();
    assert_eq!(fx.text("big.txt"), "short\nreplaced\n");
}

#[tokio::test]
async fn unseen_and_out_of_range_lines_are_reported() {
    let fx = Fx::new(&[
        ("lines.txt", "short\n0123456789abcdefgh\n"),
        ("small.txt", "a\nb\n"),
    ]);
    // 冷 byte_offset 读取不把整行计为已见
    let shown = fx
        .read(r#"{"path":"lines.txt","offset":2,"limit":1,"byte_offset":10}"#)
        .await;
    assert!(shown.contains("2:abcdefgh"), "{shown}");
    let header = shown.lines().next().unwrap();
    let error = fx
        .apply(&format!("{header}\nPUT 2.=2:\n+no\n"))
        .await
        .unwrap_err();
    assert!(error.contains("unseen"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");

    // 越界行号报不存在而非 unseen
    let error = fx
        .patch("small.txt", "PUT 99.=100:\n+z\n")
        .await
        .unwrap_err();
    assert!(
        error.contains("line 100 does not exist (file has 2 lines)"),
        "{error}"
    );
    let error = fx.patch("small.txt", "PUT >99:\n+z\n").await.unwrap_err();
    assert!(error.contains("line 99 does not exist"), "{error}");

    // 空寄存器报错点名寄存器
    let error = fx.patch("small.txt", "PUT >2 @cap\n").await.unwrap_err();
    assert!(error.contains("register @cap is empty"), "{error}");
}

#[tokio::test]
async fn paste_inside_cut_range_gets_guidance_and_outside_anchor_works() {
    let fx = Fx::new(&[("reorder.txt", "h1\n{\n    a\n    b\n}\nh2\n")]);
    let header = fx.header("reorder.txt").await;
    let error = fx
        .apply(&format!("{header}\nCUT 2* @blk\nPUT >4 @blk\n"))
        .await
        .unwrap_err();
    assert!(
        error.contains("overlap") && error.contains("inside the lines removed"),
        "{error}"
    );
    assert!(error.contains(">5"), "{error}");
    assert!(error.contains("no files were changed"), "{error}");
    fx.apply(&format!("{header}\nCUT 2* @blk\nPUT >6 @blk\n"))
        .await
        .unwrap();
    assert_eq!(fx.text("reorder.txt"), "h1\nh2\n{\n    a\n    b\n}\n");
}

#[tokio::test]
async fn grep_clips_huge_line_and_partial_read_unlocks_edit() {
    let long = "y".repeat(40 * 1024);
    let fx = Fx::new(&[("huge.txt", &format!("head\n{long}\ntail\n"))]);
    let out = fx
        .grep_tool()
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

    let patch = format!("{header}\nPUT 2.=2:\n+replaced\n");
    assert!(fx.apply(&patch).await.unwrap_err().contains("unseen"));
    fx.read(&format!(
        r#"{{"path":"huge.txt","offset":2,"limit":1,"byte_offset":{byte_offset}}}"#
    ))
    .await;
    fx.apply(&patch).await.unwrap();
    assert_eq!(fx.text("huge.txt"), "head\nreplaced\ntail\n");
}

#[tokio::test]
async fn grep_registers_fully_shown_lines() {
    let fx = Fx::new(&[("find.txt", "alpha\nbeta\ngamma\n")]);
    let out = fx
        .grep_tool()
        .execute(r#"{"pattern":"beta","path":"find.txt"}"#)
        .await
        .unwrap();
    assert!(out.contains("2:beta"), "{out}");
    let header = out.lines().next().unwrap();
    fx.apply(&format!("{header}\nPUT 2.=2:\n+BETA\n"))
        .await
        .unwrap();
    assert_eq!(fx.text("find.txt"), "alpha\nBETA\ngamma\n");
}

#[tokio::test]
async fn preview_gap_markers_match_enforcement() {
    let six = "one\ntwo\nthree\nfour\nfive\nsix\n";
    let fx = Fx::new(&[("six.txt", six)]);
    let report = fx.patch("six.txt", "PUT 1.=1:\n+ONE\n").await.unwrap();
    assert!(
        report.contains("[Lines 5-6 unchanged; shown earlier, editable without Read]"),
        "{report}"
    );
    assert!(!report.contains("Read before editing"), "{report}");
    let new_header = report.lines().next().unwrap();
    fx.apply(&format!("{new_header}\nPUT 6.=6:\n+SIX\n"))
        .await
        .unwrap();

    let fx = Fx::new(&[("six.txt", six)]);
    let shown = fx.read(r#"{"path":"six.txt","offset":1,"limit":2}"#).await;
    let header = shown.lines().next().unwrap();
    let report = fx
        .apply(&format!("{header}\nPUT 2.=2:\n+TWO\n"))
        .await
        .unwrap();
    assert!(
        report.contains("not displayed this session; Read before editing]"),
        "{report}"
    );
    let new_header = report.lines().next().unwrap();
    let error = fx
        .apply(&format!("{new_header}\nPUT 6.=6:\n+SIX\n"))
        .await
        .unwrap_err();
    assert!(error.contains("unseen"), "{error}");
}

#[tokio::test]
async fn append_to_empty_file_adds_trailing_newline() {
    let fx = Fx::new(&[("empty.txt", "")]);
    fx.patch("empty.txt", "PUT >$:\n+first\n").await.unwrap();
    assert_eq!(fx.text("empty.txt"), "first\n");
}

#[tokio::test]
async fn bom_survives_line_one_replacement() {
    let fx = Fx::new(&[("bom.txt", "\u{feff}first\nsecond\n")]);
    fx.patch("bom.txt", "PUT 1.=1:\n+FIRST\n").await.unwrap();
    assert_eq!(fx.text("bom.txt"), "\u{feff}FIRST\nsecond\n");
}

#[tokio::test]
async fn stale_tag_printed_in_error_is_reusable() {
    let fx = Fx::new(&[("st.txt", "a\nb\n")]);
    let header = fx.header("st.txt").await;
    fs::write(fx.dir.path().join("st.txt"), "a\nb\nEXTERNAL\n").unwrap();
    let error = fx
        .apply(&format!("{header}\nPUT 1.=1:\n+A\n"))
        .await
        .unwrap_err();
    assert!(error.contains("stale"), "{error}");
    let tag = error.split("(now [st.txt#").nth(1).unwrap()[..4].to_string();
    let error = fx
        .apply(&format!("[st.txt#{tag}]\nPUT 1.=1:\n+A\n"))
        .await
        .unwrap_err();
    assert!(error.contains("unseen"), "{error}");
    assert!(!error.contains("not registered"), "{error}");
}
