use super::diff::*;
use super::pipeline::*;
use super::selection::*;
use super::state::*;
use super::tool_block::*;
use super::user_block::*;
use super::view::*;
use super::*;
use crate::model::{ToolCall, ToolResult, WebSearchSource};
use crate::ui::app::{TextCoord, ToolExpand};
use crate::ui::test_support::TestApp;
use std::collections::HashMap;
use std::sync::Arc;

fn joined(lines: &[Line]) -> String {
    lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 渲染输入环境（宽度 / 展开状态 / 工具结果），默认 80 列、完整思考展示
struct Env<'r> {
    width: usize,
    concise: bool,
    results: HashMap<String, &'r ToolResult>,
    expand: HashMap<String, ToolExpand>,
    thought_expand: HashMap<String, bool>,
    citations_expand: HashMap<String, bool>,
}

impl Default for Env<'_> {
    fn default() -> Self {
        Self {
            width: 80,
            concise: false,
            results: HashMap::new(),
            expand: HashMap::new(),
            thought_expand: HashMap::new(),
            citations_expand: HashMap::new(),
        }
    }
}

/// 渲染产物槽：各输出向量集中持有，`render` 按需借出 `ChatRender`
#[derive(Default)]
struct Sink {
    lines: Vec<Line<'static>>,
    links: Vec<LinkRegion>,
    hits: Vec<ToolHit>,
    thought_hits: Vec<ThoughtHit>,
    citations_hits: Vec<CitationsHit>,
    response_hits: Vec<ResponseHit>,
}

impl Sink {
    fn render<'a>(&'a mut self, env: &'a Env<'_>) -> ChatRender<'a> {
        ChatRender {
            notices: &[],
            lines: &mut self.lines,
            links: &mut self.links,
            max_width: env.width,
            thinking_concise: env.concise,
            results: &env.results,
            expand: &env.expand,
            hits: &mut self.hits,
            thought_expand: &env.thought_expand,
            thought_hits: &mut self.thought_hits,
            citations_expand: &env.citations_expand,
            citations_hits: &mut self.citations_hits,
            response_hits: &mut self.response_hits,
        }
    }

    /// 以默认环境渲染一组部件
    fn append<'p>(&mut self, role: Role, parts: impl Iterator<Item = &'p ContentPart> + Clone) {
        self.append_with(&Env::default(), role, parts);
    }

    fn append_with<'p>(
        &mut self,
        env: &Env<'_>,
        role: Role,
        parts: impl Iterator<Item = &'p ContentPart> + Clone,
    ) {
        append_parts(
            &mut self.render(env),
            role,
            parts,
            &mut RenderState::default(),
        );
    }

    fn text(&self) -> String {
        joined(&self.lines)
    }
}

/// 各行 Span 文本直接拼接（不含换行）
fn plain(lines: &[Line]) -> String {
    lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect()
}

#[test]
fn edit_detail_uses_unified_diff_when_narrow() {
    let mut lines = Vec::new();
    push_edit_detail(
        &mut lines,
        r#"{"path":"a.rs","old_string":"old","new_string":"new"}"#,
        None,
        40,
        20,
    );
    let text = joined(&lines);
    assert!(text.contains("   1 - old"));
    assert!(text.contains("   1 + new"));
    assert!(!text.contains("│"));
}

#[test]
fn edit_detail_uses_columns_when_wide() {
    let mut lines = Vec::new();
    push_edit_detail(
        &mut lines,
        r#"{"path":"a.rs","old_string":"old","new_string":"new"}"#,
        None,
        80,
        20,
    );
    let text = joined(&lines);
    assert!(text.contains("   1 - old"));
    assert!(text.contains("   1 + new"));
    assert!(text.contains("│"));
}

#[test]
fn edit_detail_uses_standard_line_backgrounds() {
    let mut lines = Vec::new();
    push_edit_detail(
        &mut lines,
        r#"{"path":"a.rs","old_string":"same\nold","new_string":"same\nnew"}"#,
        None,
        40,
        20,
    );

    let context = lines
        .iter()
        .find(|line| line.to_string().contains("same"))
        .unwrap();
    let deleted = lines
        .iter()
        .find(|line| line.to_string().contains("   2 - old"))
        .unwrap();
    let inserted = lines
        .iter()
        .find(|line| line.to_string().contains("   2 + new"))
        .unwrap();

    assert!(context.spans.iter().all(|span| span.style.bg.is_none()));
    assert!(
        deleted
            .spans
            .iter()
            .all(|span| span.style.bg == Some(EDIT_DELETE_BG))
    );
    assert!(
        inserted
            .spans
            .iter()
            .all(|span| span.style.bg == Some(EDIT_INSERT_BG))
    );
}

#[test]
fn edit_detail_uses_hunk_line_numbers() {
    let mut lines = Vec::new();
    push_edit_detail(
        &mut lines,
        r#"{"path":"a.rs","old_string":"same\nold","new_string":"same\nnew"}"#,
        Some("--- a.rs\n+++ a.rs\n@@ -308,2 +308,2 @@\n same\n-old\n+new"),
        80,
        20,
    );

    let text = joined(&lines);
    assert!(text.contains(" 308   same"));
    assert!(text.contains(" 309 - old"));
    assert!(text.contains(" 309 + new"));
}

fn render_tools(
    parts: &[ContentPart],
    results: &HashMap<String, &ToolResult>,
    expand: &HashMap<String, ToolExpand>,
    width: usize,
) -> (Vec<Line<'static>>, Vec<ToolHit>) {
    let env = Env {
        width,
        concise: true,
        results: results.clone(),
        expand: expand.clone(),
        ..Env::default()
    };
    let mut sink = Sink::default();
    sink.append_with(&env, Role::Assistant, parts.iter());
    (sink.lines, sink.hits)
}

/// 渲染单个工具调用：返回文本、行与命中区
fn render_call(
    call: &ToolCall,
    result: Option<&ToolResult>,
    expand: &[(&str, ToolExpand)],
) -> (String, Vec<ToolHit>) {
    let results: HashMap<_, _> = result
        .map(|r| (r.tool_call_id.clone(), r))
        .into_iter()
        .collect();
    let expand = expand
        .iter()
        .map(|(id, mode)| (id.to_string(), *mode))
        .collect();
    let (lines, hits) = render_tools(
        &[ContentPart::ToolCall(call.clone())],
        &results,
        &expand,
        80,
    );
    (joined(&lines), hits)
}

#[test]
fn tool_call_collapsed_is_one_line() {
    let call = ToolCall::new("c1", "Read", r#"{"path":"README.md"}"#);
    let result = ToolResult::success("c1", "1: hello\n2: world");
    let (text, _) = render_call(&call, Some(&result), &[]);
    assert!(text.contains("* Read README.md [offset=1, limit=all]"));
    for hidden in ["→ Read", "hello", "Output:"] {
        assert!(!text.contains(hidden));
    }
}

#[test]
fn tool_read_shows_requested_range() {
    let call = ToolCall::new(
        "c1",
        "Read",
        r#"{"path":"src/main.rs","offset":10,"limit":20}"#,
    );
    let (text, _) = render_call(&call, Some(&ToolResult::success("c1", "10: hello")), &[]);
    assert!(text.contains("* Read src/main.rs [offset=10, limit=20]"));
}

#[test]
fn edit_header_colors_counts_and_block_omits_path() {
    let call = ToolCall::new(
        "c1",
        "Edit",
        r#"{"path":"src/main.rs","old_string":"old\nline","new_string":"new\nlines\nhere"}"#,
    );
    let result = ToolResult::success("c1", "Done");
    let mut results = HashMap::new();
    results.insert(result.tool_call_id.clone(), &result);
    let expand = HashMap::new();

    let (lines, _) = render_tools(&[ContentPart::ToolCall(call)], &results, &expand, 80);

    assert_eq!(joined(&lines).matches("src/main.rs").count(), 1);
    assert!(!joined(&lines).contains("Click to expand"));
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| { span.content == "+3" && span.style.fg == EDIT_INSERT.fg })
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| { span.content == "-2" && span.style.fg == EDIT_DELETE.fg })
    );
}

#[test]
fn write_defaults_to_full_content_and_can_hide() {
    let content = (1..=12).map(|i| format!("row {i}\n")).collect::<String>();
    let call = ToolCall::new(
        "w1",
        "Write",
        serde_json::json!({"path": "example.txt", "content": content}).to_string(),
    );
    let parts = [ContentPart::ToolCall(call)];
    let (lines, hits) = render_tools(&parts, &HashMap::new(), &HashMap::new(), 80);
    let text = joined(&lines);
    assert!(text.contains("row 12"));
    assert_eq!(text.matches("example.txt").count(), 1);
    assert!(!text.contains("Click to expand"));
    assert!(hits[0].full_only);
    assert!(!hits[0].truncated);

    let expand = HashMap::from([("w1".to_string(), ToolExpand::Hidden)]);
    let (lines, _) = render_tools(&parts, &HashMap::new(), &expand, 80);
    assert!(!joined(&lines).contains("row 1"));

    let result = ToolResult::error("w1", "write failed");
    let results = HashMap::from([("w1".to_string(), &result)]);
    let (lines, _) = render_tools(&parts, &results, &HashMap::new(), 80);
    assert!(joined(&lines).contains("write failed"));
    assert!(!joined(&lines).contains("row 1"));
}

#[test]
fn tool_glob_shows_match_count() {
    let call = ToolCall::new("c1", "Glob", r#"{"pattern":"**/*.rs"}"#);
    let result = ToolResult::success("c1", "a.rs\nb.rs\nc.rs");
    let (text, _) = render_call(&call, Some(&result), &[]);
    assert!(text.contains("* Glob \"**/*.rs\" (3 matches)"));
    assert!(!text.contains("→ Glob"));
}

#[test]
fn tool_shell_defaults_to_preview() {
    let call = ToolCall::new(
        "c1",
        "Shell",
        r#"{"intent":"count lines","command":"wc -l src/*.ts"}"#,
    );
    let result = ToolResult::success("c1", "26 src/result.ts\n30 src/types.ts");
    let (text, _) = render_call(&call, Some(&result), &[]);
    for expected in [
        "↓ Shell count lines",
        "$ wc -l src/*.ts",
        "26 src/result.ts",
    ] {
        assert!(text.contains(expected), "{expected}");
    }
}

#[test]
fn tool_shell_multiline_command_keeps_lines() {
    let call = ToolCall::new(
        "c1",
        "Shell",
        r#"{"intent":"capture","command":"rm -f /tmp/a.txt\nsleep 8\ncat /tmp/a.txt"}"#,
    );
    let (text, _) = render_call(&call, None, &[]);
    for expected in ["$ rm -f /tmp/a.txt", "sleep 8", "cat /tmp/a.txt"] {
        assert!(text.contains(expected), "{expected}");
    }
    // 相邻命令不得因控制字符被过滤而拼接
    assert!(!text.contains("txtsleep"));
    assert!(!text.contains("8cat"));
}

#[test]
fn tool_preview_block_truncates_and_full_shows_all() {
    let output: String = (1..=15)
        .map(|n| format!("line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let call = ToolCall::new("c1", "Shell", r#"{"intent":"list","command":"seq 15"}"#);
    let result = ToolResult::success("c1", output);

    let (text, hits) = render_call(&call, Some(&result), &[("c1", ToolExpand::Preview)]);
    for expected in [
        "↓ Shell list",
        "$ seq 15",
        "line1",
        "line10",
        "Click to expand",
    ] {
        assert!(text.contains(expected), "{expected}");
    }
    assert!(!text.contains("line11"));
    assert_eq!(hits.len(), 1);
    assert!(hits[0].truncated);

    let (text, hits) = render_call(&call, Some(&result), &[("c1", ToolExpand::Full)]);
    assert!(text.contains("line15"));
    assert!(!text.contains("Click to expand"));
    assert!(!hits[0].truncated);
}

#[test]
fn background_task_reminder_not_rendered() {
    let text = crate::system::format_background_task_reminder(
        "Background task 1 exited with code 0\nintent: watch build\ncaptured output: 5 bytes",
    );
    let mut sink = Sink::default();
    sink.append(Role::User, [ContentPart::text(text)].iter());
    let lines = sink.lines;

    // 后台任务完成通知不渲染（完成状态在工具块展示），也不落入用户引用块
    let rendered = joined(&lines);
    assert!(
        !rendered.contains("Background task 1 exited with code 0"),
        "{rendered}"
    );
    assert!(!rendered.contains("intent: watch build"), "{rendered}");
    assert!(!rendered.contains("system-reminder"), "{rendered}");
    assert!(
        lines
            .iter()
            .all(|line| { line.spans.iter().all(|span| !span.content.contains('▌')) })
    );
}

#[test]
fn test_user_block_copy_excludes_decoration() {
    let mut lines = Vec::new();
    push_user_block(
        &mut lines,
        "第一行示例 https://example.com\n第二行混合文本",
        80,
    );
    let start = TextCoord { line: 0, col: 0 };
    let end = TextCoord {
        line: lines.len() - 1,
        col: 80,
    };
    for selection in [
        TextSelection::new(start, end),
        TextSelection::new(end, start),
    ] {
        assert_eq!(
            extract_selected_text(&lines, &selection).as_deref(),
            Some("第一行示例 https://example.com\n第二行混合文本")
        );
    }
    let selection =
        TextSelection::new(TextCoord { line: 1, col: 0 }, TextCoord { line: 1, col: 4 });
    assert_eq!(
        extract_selected_text(&lines, &selection).as_deref(),
        Some("")
    );
}

fn build_chat_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let env = Env {
        width: width.max(20),
        concise: app.thinking_concise,
        results: collect_tool_results(&app.view),
        expand: app.tool_expand.clone(),
        thought_expand: app.thought_expand.clone(),
        citations_expand: app.citations_expand.clone(),
    };
    let mut sink = Sink::default();
    let mut state = RenderState::default();
    for turn in &app.view.session.turns {
        append_parts(
            &mut sink.render(&env),
            turn.role,
            turn.messages.iter().flat_map(|m| &m.parts),
            &mut state,
        );
    }
    if is_empty_session(&app.view) {
        push_empty_hint(&mut sink.render(&env));
    }
    let draft_ctx = part_context(app.view.draft.iter().map(|part| &**part));
    for part in &app.view.draft {
        append_part(
            &mut sink.render(&env),
            Role::Assistant,
            part,
            &draft_ctx,
            &mut state,
        );
    }
    if state.prev_text {
        sink.lines.push(Line::from(""));
    }
    sink.lines
}

async fn create_test_app() -> TestApp {
    crate::ui::test_support::create_test_app("init_model", None, None, false).await
}

#[tokio::test]
async fn test_chat_empty_placeholder() {
    let app = create_test_app().await;
    let lines = build_chat_lines(&app, 80);
    assert!(lines.len() >= 2);
    let joined = plain(&lines);
    assert!(joined.contains("Start typing a prompt"));
}

#[tokio::test]
async fn test_chat_cache_preserves_history_and_layout() {
    let mut ta = create_test_app().await;
    let app = &mut ta.app;
    let check = |app: &mut App, width| {
        let expected = build_chat_lines(app, width);
        app.refresh_chat_lines(width);
        assert_eq!(app.chat.lines, expected);
    };
    check(app, 80);
    app.add_part(
        Role::User,
        ContentPart::text("History 中文与 English words ".repeat(4)),
    );
    app.add_part(
        Role::Assistant,
        ContentPart::reasoning("**First thought**", None),
    );
    check(app, 80);
    let history_text = |app: &App| {
        app.chat
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("History"))
            .unwrap()
            .content
            .as_ptr()
    };
    let history = history_text(app);
    for draft in [
        vec![Arc::new(ContentPart::reasoning("**Next thought**", None))],
        vec![
            Arc::new(ContentPart::reasoning("**Next thought**", None)),
            Arc::new(ContentPart::text("**Answer**")),
        ],
        vec![],
    ] {
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft = draft;
        check(app, 80);
        assert_eq!(history_text(app), history);
    }
    app.feedback.push("Local notice".into());
    check(app, 80);
    app.feedback.clear();
    check(app, 80);
    assert_eq!(history_text(app), history);

    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    view.draft
        .push(Arc::new(ContentPart::reasoning("**Next thought**", None)));
    for concise in [false, true] {
        app.thinking_concise = concise;
        for width in [30, 120, 80] {
            check(app, width);
        }
    }
    // 会话内容替换时，即使轮次数与视图版本相同也必须重建历史。
    let session = Arc::make_mut(&mut Arc::make_mut(&mut app.view).session);
    session.turns[0] = crate::model::Turn::user_turn(crate::model::Message::text("Replacement"));
    check(app, 80);
    app.attach_session(app.active_session.clone());
    check(app, 80);
    assert!(
        app.chat
            .lines
            .iter()
            .any(|line| line.to_string().contains("Start typing"))
    );
}

#[tokio::test]
async fn test_draft_stream_keeps_committed_prefix_and_matches_full_render() {
    let mut ta = create_test_app().await;
    let app = &mut ta.app;
    let check = |app: &mut App, width| {
        let expected = build_chat_lines(app, width);
        app.refresh_chat_lines(width);
        assert_eq!(app.chat.lines, expected);
    };
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    view.draft = vec![Arc::new(ContentPart::text("第一段。\n\n第二段开始"))];
    check(app, 80);
    let prefix = app
        .chat
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.contains("第一段"))
        .unwrap()
        .content
        .as_ptr();
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    if let ContentPart::Text { text, .. } = Arc::make_mut(&mut view.draft[0]) {
        text.push_str("，续写。");
    }
    check(app, 80);
    let prefix_after = app
        .chat
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.contains("第一段"))
        .unwrap()
        .content
        .as_ptr();
    assert_eq!(prefix, prefix_after);
}

#[tokio::test]
async fn test_thinking_grouping_and_concise_modes() {
    let mut app = create_test_app().await;
    app.thinking_concise = false;
    // 模拟协议层输出：各 summary part 带原始 markdown 标记，以空行（段落边界）分隔
    app.add_part(
        Role::Assistant,
        ContentPart::reasoning(
            "**Collecting example sources**\n\n**Drafting example response**".to_string(),
            None,
        ),
    );

    // Full mode keeps all paragraphs under one heading.
    let lines = build_chat_lines(&app, 80);
    let thought_lines: Vec<_> = lines
        .iter()
        .filter(|l| l.spans.first().is_some_and(|s| s.content == "- Thought"))
        .collect();
    assert_eq!(thought_lines.len(), 1);
    assert_eq!(thought_lines[0].to_string(), "- Thought");
    assert!(
        !thought_lines[0]
            .spans
            .first()
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    let first_body = lines
        .iter()
        .find(|l| l.to_string().contains("Collecting"))
        .unwrap();
    assert_eq!(first_body.to_string(), "  Collecting example sources");

    // markdown 标记被渲染掉，加粗修饰保留，两段分行
    let all_text = plain(&lines);
    assert!(all_text.contains("Collecting example sources"));
    assert!(all_text.contains("Drafting example response"));
    assert!(!all_text.contains("**"));
    let bold_in_first = first_body
        .spans
        .iter()
        .any(|s| s.content.contains("Collecting") && s.style.add_modifier.contains(Modifier::BOLD));
    assert!(bold_in_first);

    // 追加搜索调用与第二个思考 part：搜索块渲染为分组边界，前后思考各自成段。
    // 搜索调用、思考与带注脚的正文同属一轮，来源由同轮注脚提供。
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
        Role::Assistant,
        vec![
            crate::model::Message::new(vec![ContentPart::WebSearchCall(WebSearchCall::completed(
                "ws_1",
                "示例查询",
                vec![WebSearchSource::new(
                    "https://source.example/article",
                    "Uncited candidate",
                    None,
                )],
            ))]),
            crate::model::Message::new(vec![ContentPart::reasoning(
                "**Cross-checking sources**".to_string(),
                None,
            )]),
            crate::model::Message::new(vec![ContentPart::Text {
                text: "这里是带引用的示例答复。".to_string(),
                annotations: vec![Annotation::UrlCitation {
                    title: "示例来源".into(),
                    url: "https://source.example/article".into(),
                    start_index: 0,
                    end_index: 1,
                }],
            }]),
        ],
    ));
    let lines = build_chat_lines(&app, 80);
    let header_idx: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.spans.first().is_some_and(|s| s.content == "- Thought"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(header_idx.len(), 1);
    let inline_idx = lines
        .iter()
        .position(|l| l.spans.first().is_some_and(|s| s.content == "Thought:"))
        .expect("single-line thought rendered inline");
    assert_eq!(
        lines[inline_idx].to_string(),
        "Thought: Cross-checking sources"
    );
    // 搜索块出现在两组思考之间，来源行左侧为注脚补齐的网页标题、域名右对齐至可用宽度
    let search_idx = lines
        .iter()
        .position(|l| l.spans.iter().any(|s| s.content == "web_search"))
        .expect("web search block rendered");
    assert!(search_idx > header_idx[0] && search_idx < inline_idx);
    let source_row = &lines[search_idx + 1];
    assert!(
        source_row
            .spans
            .iter()
            .any(|s| s.content.contains("示例来源"))
    );
    let last = source_row.spans.last().unwrap();
    assert_eq!(last.content, "source.example");
    let row_width: usize = source_row.spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(row_width, 80);

    // 轮次内已有搜索调用：正文后的末尾来源汇总不再渲染
    let all_text = plain(&lines);
    assert!(all_text.contains("这里是带引用的示例答复"));
    assert!(all_text.contains("示例查询 (1 sources)"));
    assert!(!all_text.contains("candidate"));
    assert!(!all_text.contains("Sources / Citations"));

    // concise 模式：多段思考折叠为单行标记；单行思考保持 Thought: 正文
    app.thinking_concise = true;
    let lines = build_chat_lines(&app, 80);
    let headers = lines
        .iter()
        .filter(|l| l.spans.first().is_some_and(|s| s.content == "+ Thought"))
        .count();
    assert_eq!(headers, 1);
    let all_text = plain(&lines);
    assert!(!all_text.contains("Collecting example sources"));
    assert!(all_text.contains("Thought:"));
    assert!(all_text.contains("Cross-checking sources"));
    assert!(!all_text.contains("**"));
}

#[tokio::test]
async fn test_single_line_thought_renders_inline() {
    let mut app = create_test_app().await;
    app.add_part(
        Role::Assistant,
        ContentPart::reasoning("Researching ARM Granular Data Isolation", None),
    );
    for concise in [false, true] {
        app.thinking_concise = concise;
        let lines = build_chat_lines(&app, 80);
        let thought = lines
            .iter()
            .find(|l| l.spans.first().is_some_and(|s| s.content == "Thought:"))
            .expect("inline thought line");
        assert_eq!(
            thought.to_string(),
            "Thought: Researching ARM Granular Data Isolation"
        );
        let markers = lines.iter().any(|l| {
            l.spans
                .first()
                .is_some_and(|s| s.content == "- Thought" || s.content == "+ Thought")
        });
        assert!(!markers);
    }
}

#[tokio::test]
async fn test_prose_thought_stays_collapsible() {
    let mut app = create_test_app().await;
    app.add_part(
        Role::Assistant,
        ContentPart::reasoning(
            "There's a zip file called \"地下酒吧 1.0.2.zip\" which is a Minecraft modpack. Let me extract it and examine its contents.",
            None,
        ),
    );
    app.thinking_concise = true;
    let lines = build_chat_lines(&app, 80);
    let all_text = plain(&lines);
    assert!(all_text.contains("+ Thought"));
    assert!(!all_text.contains("Thought:"));
    assert!(!all_text.contains("地下酒吧"));

    app.thinking_concise = false;
    let lines = build_chat_lines(&app, 80);
    let all_text = plain(&lines);
    assert!(all_text.contains("- Thought"));
    assert!(!all_text.contains("Thought:"));
    assert!(all_text.contains("地下酒吧"));
}

#[tokio::test]
async fn test_thought_group_click_toggles_individual_group() {
    let mut app = create_test_app().await;
    app.add_part(
        Role::Assistant,
        ContentPart::reasoning("First group body\n\nMore detail", None),
    );
    app.add_part(Role::Assistant, ContentPart::text("Middle answer"));
    app.add_part(
        Role::Assistant,
        ContentPart::reasoning("Second group body\n\nMore detail", None),
    );

    app.refresh_chat_lines(80);
    let headers: Vec<(String, usize)> = app
        .chat_cache
        .thought_hits
        .iter()
        .map(|hit| (hit.id.clone(), hit.header_start))
        .collect();
    assert_eq!(
        headers
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["thought-1", "thought-2"]
    );
    let text = |app: &App| -> String {
        app.chat
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect()
    };
    assert!(!text(&app).contains("First group body"));
    assert!(!text(&app).contains("Second group body"));

    // 展开第一组：仅该组正文可见，标题切换为展开标记
    assert!(app.handle_thought_click(headers[0].1));
    app.refresh_chat_lines(80);
    let rendered = text(&app);
    assert!(rendered.contains("First group body"));
    assert!(!rendered.contains("Second group body"));
    assert!(
        app.chat
            .lines
            .iter()
            .any(|l| { l.spans.first().is_some_and(|s| s.content == "- Thought") })
    );

    // 点击正文区域折叠：覆盖值回到全局默认时移除条目
    let first = app.chat_cache.thought_hits[0].clone();
    assert!(first.body_end > first.body_start);
    assert!(app.handle_thought_click(first.body_start));
    app.refresh_chat_lines(80);
    assert!(!text(&app).contains("First group body"));
    assert!(app.thought_expand.is_empty());

    // 全局切换到 Full 清空既有覆盖，两组正文均可见
    assert!(app.handle_thought_click(headers[1].1));
    app.toggle_thinking_display();
    assert!(app.thought_expand.is_empty());
    app.refresh_chat_lines(80);
    let rendered = text(&app);
    assert!(rendered.contains("First group body"));
    assert!(rendered.contains("Second group body"));
}

#[tokio::test]
async fn test_trailing_citations_fold_and_click_to_expand() {
    let mut app = create_test_app().await;
    app.add_part(
        Role::Assistant,
        ContentPart::Text {
            text: "这里是带引用的示例答复。".to_string(),
            annotations: vec![
                Annotation::UrlCitation {
                    title: "示例来源".into(),
                    url: "https://source.example/article".into(),
                    start_index: 0,
                    end_index: 1,
                },
                Annotation::UrlCitation {
                    title: "另一个来源".into(),
                    url: "https://other.example/page".into(),
                    start_index: 0,
                    end_index: 1,
                },
            ],
        },
    );

    app.refresh_chat_lines(80);
    let text = |app: &App| -> String {
        app.chat
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect()
    };
    // 默认折叠：仅标题行带计数，来源列表不可见
    assert!(text(&app).contains("+ Sources / Citations (2)"));
    assert!(!text(&app).contains("source.example"));
    assert_eq!(app.chat_cache.citations_hits.len(), 1);
    assert_eq!(app.chat_cache.citations_hits[0].id, "citations-1");

    // 单击标题行展开：来源列表可见，标题切换为展开标记
    let header = app.chat_cache.citations_hits[0].header_start;
    assert!(app.handle_citations_click(header));
    app.refresh_chat_lines(80);
    let rendered = text(&app);
    assert!(rendered.contains("- Sources / Citations (2)"));
    assert!(rendered.contains("[1] 示例来源"));
    assert!(rendered.contains("[2] 另一个来源"));

    // 再次单击折叠：覆盖值直接记录折叠态
    let header = app.chat_cache.citations_hits[0].header_start;
    assert!(app.handle_citations_click(header));
    app.refresh_chat_lines(80);
    assert!(text(&app).contains("+ Sources / Citations (2)"));
    assert!(!text(&app).contains("source.example"));
}

#[tokio::test]
async fn test_separate_search_calls_track_draft_citations_and_turn_boundaries() {
    let mut app = create_test_app().await;
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    view.draft = vec![
        Arc::new(ContentPart::text("Before search")),
        Arc::new(ContentPart::WebSearchCall(WebSearchCall::completed(
            "ws_1",
            "query A; query A2",
            vec![
                WebSearchSource::new("https://candidate.example/page", "Uncited candidate", None),
                WebSearchSource::new(
                    "https://www.cited.example/page",
                    "Source title must not appear",
                    None,
                ),
            ],
        ))),
        Arc::new(ContentPart::text("Between searches")),
        Arc::new(ContentPart::WebSearchCall(WebSearchCall::completed(
            "ws_2",
            "query B",
            vec![
                WebSearchSource::new("https://www.cited.example/page", "Ignored", None),
                WebSearchSource::new("https://other.example/page", "Ignored", None),
            ],
        ))),
    ];
    app.refresh_chat_lines(100);
    let text = |lines: &[Line<'_>]| -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect()
    };
    let initial = text(&app.chat.lines);
    assert_eq!(initial.matches("web_search").count(), 2);
    assert!(initial.contains("web_search query A; query A2 (2 sources)"));
    assert!(initial.contains("web_search query B (2 sources)"));
    assert!(!initial.contains("candidate"));
    assert!(initial.find("Before search").unwrap() < initial.find("web_search").unwrap());
    assert!(initial.find("web_search").unwrap() < initial.find("Between searches").unwrap());
    assert!(initial.find("Between searches").unwrap() < initial.find("query B").unwrap());

    let citation = Annotation::UrlCitation {
        url: "https://www.cited.example/page".into(),
        title: "Provided title".into(),
        start_index: 0,
        end_index: 1,
    };
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    view.draft.push(Arc::new(ContentPart::Text {
        text: "Answer".into(),
        annotations: vec![citation.clone()],
    }));
    app.refresh_chat_lines(100);
    let updated = text(&app.chat.lines);
    assert!(updated.contains("query A; query A2 (2 sources)"));
    assert!(updated.contains("query B (2 sources)"));
    assert_eq!(updated.matches("Provided title").count(), 2);
    assert!(updated.contains("cited.example"));
    assert!(!updated.contains("candidate"));

    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    view.draft.push(Arc::new(ContentPart::Text {
        text: "More".into(),
        annotations: vec![
            citation,
            Annotation::UrlCitation {
                url: "https://other.example/page".into(),
                title: String::new(),
                start_index: 0,
                end_index: 1,
            },
            Annotation::UrlCitation {
                url: "https://unmatched.example/page".into(),
                title: "Unmatched reference".into(),
                start_index: 0,
                end_index: 1,
            },
        ],
    }));
    // 末尾来源汇总默认折叠；展开后断言未命中调用覆盖的引用列表
    app.citations_expand.insert("citations-1".into(), true);
    app.refresh_chat_lines(100);
    let rendered = text(&app.chat.lines);
    assert_eq!(rendered.matches("web_search").count(), 2);
    assert!(rendered.contains("query A; query A2 (2 sources)"));
    assert!(rendered.contains("query B (2 sources)"));
    assert_eq!(rendered.matches("Provided title").count(), 2);
    let first_group = rendered.split("Between searches").next().unwrap();
    assert!(first_group.contains("Provided title"));
    assert!(!first_group.contains("other.example"));
    let second_group = rendered.split("query B (2 sources)").nth(1).unwrap();
    assert!(second_group.contains("Provided title"));
    assert!(second_group.contains("other.example"));
    assert!(!rendered.contains("Source title must not appear"));
    assert!(!rendered.contains("Ignored"));
    let listing = rendered.split("Sources / Citations").nth(1).unwrap();
    assert!(listing.contains("Unmatched reference - https://unmatched.example/page"));
    assert!(!listing.contains("Provided title"));
    assert!(!listing.contains("other.example"));

    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    let messages = std::mem::take(&mut view.draft)
        .into_iter()
        .map(|part| crate::model::Message::new(vec![Arc::unwrap_or_clone(part)]))
        .collect();
    Arc::make_mut(&mut view.session)
        .append_turn(crate::model::Turn::new(Role::Assistant, messages));
    app.refresh_chat_lines(100);
    assert_eq!(text(&app.chat.lines), rendered);

    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    view.draft.push(Arc::new(ContentPart::WebSearchCall(
        WebSearchCall::completed(
            "ws_3",
            "query C",
            vec![WebSearchSource::new(
                "https://www.cited.example/page",
                "Ignored",
                None,
            )],
        ),
    )));
    app.refresh_chat_lines(100);
    let separate = text(&app.chat.lines);
    assert_eq!(separate.matches("web_search").count(), 3);
    assert!(separate.contains("query A; query A2 (2 sources)"));
    assert!(separate.contains("query C (1 sources)"));
    assert_eq!(separate.matches("Provided title").count(), 2);
    assert!(
        !separate
            .split("query C (1 sources)")
            .nth(1)
            .unwrap()
            .contains("Provided title")
    );
}

#[tokio::test]
async fn test_open_and_find_match_action_urls() {
    let mut app = create_test_app().await;
    let url = "https://www.example.com/page";
    let mut open = WebSearchCall::completed("open", "unused query", vec![]);
    open.action = Some(WebSearchAction::OpenPage { url: url.into() });
    let mut find = WebSearchCall::completed("find", "unused query", vec![]);
    find.action = Some(WebSearchAction::FindInPage {
        url: url.into(),
        pattern: "matching words".into(),
    });
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    view.draft = vec![
        Arc::new(ContentPart::WebSearchCall(open)),
        Arc::new(ContentPart::text("Between actions")),
        Arc::new(ContentPart::WebSearchCall(find)),
        Arc::new(ContentPart::Text {
            text: "Answer".into(),
            annotations: vec![
                Annotation::UrlCitation {
                    url: url.into(),
                    title: "Annotation title".into(),
                    start_index: 0,
                    end_index: 1,
                },
                Annotation::UrlCitation {
                    url: "https://unmatched.example/page".into(),
                    title: "Unmatched title".into(),
                    start_index: 0,
                    end_index: 1,
                },
            ],
        }),
    ];
    // 末尾来源汇总默认折叠；展开后断言未命中调用覆盖的引用列表
    app.citations_expand.insert("citations-1".into(), true);
    let lines = build_chat_lines(&app, 100);
    let text: String = lines.iter().map(|line| line.to_string()).collect();
    assert!(text.contains(&format!("web_open {url}")));
    assert!(text.contains(&format!("web_find {url} / matching words")));
    assert!(!text.contains("sources)"));
    assert!(!text.contains("unused query"));
    assert_eq!(text.matches("Annotation title").count(), 1);
    let open_group = text.split("Between actions").next().unwrap();
    assert!(!open_group.contains("Annotation title"));
    assert!(!open_group.contains("Unmatched title"));
    let find_group = text
        .split("web_find")
        .nth(1)
        .unwrap()
        .split("Answer")
        .next()
        .unwrap();
    assert!(find_group.contains("Annotation title"));
    assert!(!find_group.contains("Unmatched title"));
    let listing = text.split("Sources / Citations").nth(1).unwrap();
    assert!(listing.contains("Unmatched title"));
    assert!(!listing.contains("Annotation title"));
    for width in [20, 30] {
        let narrow = build_chat_lines(&app, width);
        let source_rows: Vec<_> = narrow
            .iter()
            .filter(|line| {
                line.spans.len() == 4
                    && line.spans.last().unwrap().style.fg == Some(Color::DarkGray)
            })
            .collect();
        assert_eq!(source_rows.len(), 1);
        for line in source_rows {
            assert_eq!(line.width(), width);
        }
    }
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    let parts = std::mem::take(&mut view.draft)
        .into_iter()
        .map(Arc::unwrap_or_clone)
        .collect::<Vec<_>>();
    Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
        Role::Assistant,
        vec![crate::model::Message::new(parts)],
    ));
    assert_eq!(build_chat_lines(&app, 100), lines);
}

#[tokio::test]
async fn test_user_message_block_rendering() {
    let mut app = create_test_app().await;
    app.add_part(Role::User, ContentPart::text("hello"));
    let lines = build_chat_lines(&app, 80);
    let block_rows: Vec<_> = lines
        .iter()
        .filter(|l| l.spans.first().is_some_and(|s| s.content == "▌"))
        .collect();
    // 上下留白行与内容行均以色条起头，整行填充块底色至可用宽度
    assert!(block_rows.len() >= 3);
    for row in &block_rows {
        let width: usize = row.spans.iter().map(|s| s.content.width()).sum();
        assert_eq!(width, 80);
        assert!(row.spans.iter().any(|s| s.style.bg == Some(USER_BLOCK_BG)));
    }
}

#[tokio::test]
async fn test_user_file_ref_and_attachment_rows() {
    let mut app = create_test_app().await;
    // 单个用户轮次：Text 之后依次跟随 FileRef / Image / File 附件
    let view = Arc::make_mut(&mut app.view);
    view.revision += 1;
    Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
        Role::User,
        vec![crate::model::Message::new(vec![
            ContentPart::text("看下这个文件"),
            ContentPart::FileRef {
                path: "src/main.rs".into(),
                content: "fn main() {}\n".into(),
                truncated: false,
            },
            ContentPart::FileRef {
                path: "docs/big.md".into(),
                content: "first line\n".into(),
                truncated: true,
            },
            ContentPart::image_url("https://example.com/a.png"),
            ContentPart::File {
                source: crate::model::MediaSource::Url {
                    url: "https://example.com/a.pdf".into(),
                },
                mime_type: "application/pdf".into(),
                filename: "a.pdf".into(),
            },
        ])],
    ));

    let lines = build_chat_lines(&app, 80);
    let rendered = plain(&lines);
    // FileRef 渲染为紧凑引用行：路径 + 行数，截断标注生效，正文不倾倒
    assert!(rendered.contains("📎 src/main.rs · 1 line"), "{rendered}");
    assert!(
        rendered.contains("📎 docs/big.md · 1 line · truncated"),
        "{rendered}"
    );
    assert!(!rendered.contains("fn main"), "{rendered}");
    assert!(!rendered.contains("first line"), "{rendered}");
    // 图片与文件附件仅标注类型 / 文件名
    assert!(rendered.contains("🖼️ 图片附件"), "{rendered}");
    assert!(rendered.contains("📄 a.pdf"), "{rendered}");
    // 附件行沿用用户引用块样式（色条 + 块底色）
    let attachment_rows: Vec<_> = lines
        .iter()
        .filter(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("📎") || s.content.contains("📄"))
        })
        .collect();
    assert_eq!(attachment_rows.len(), 3);
    for row in &attachment_rows {
        assert_eq!(row.spans.first().map(|s| s.content.as_ref()), Some("▌"));
        assert!(row.spans.iter().any(|s| s.style.bg == Some(USER_BLOCK_BG)));
    }
}

#[tokio::test]
async fn test_assistant_markdown_rendering() {
    let mut app = create_test_app().await;
    let md_content = r#"# Hello World
This is **bold** text and `inline code`.

Here is a list:
- Item 1
- Item 2

```rust
fn main() {
println!("hello");
}
```
"#;
    app.add_part(
        Role::Assistant,
        ContentPart::Text {
            text: md_content.to_string(),
            annotations: vec![Annotation::UrlCitation {
                title: "Rust Lang".into(),
                url: "https://rust-lang.org".into(),
                start_index: 0,
                end_index: 1,
            }],
        },
    );

    // 展开末尾来源汇总以断言引用渲染
    app.citations_expand.insert("citations-1".into(), true);
    let lines = build_chat_lines(&app, 80);
    assert!(!lines.is_empty());

    // 检查渲染出的 Markdown 内容
    let all_text = plain(&lines);
    assert!(all_text.contains("Hello World"));
    assert!(all_text.contains("bold"));
    assert!(all_text.contains("inline code"));
    assert!(all_text.contains("Item 1"));
    assert!(all_text.contains("Item 2"));
    assert!(all_text.contains("println!"));
    assert!(all_text.contains("Rust Lang"));
    assert!(all_text.contains("https://rust-lang.org"));

    // 检查非空内容行均与 thinking 正文保持相同前缀缩进
    let assistant_lines: Vec<_> = lines
        .iter()
        .filter(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("Hello World") || s.content.contains("bold"))
        })
        .collect();
    for line in assistant_lines {
        assert_eq!(line.spans.first().map(|s| s.content.as_ref()), Some("  "));
    }
}

#[tokio::test]
async fn test_assistant_markdown_streaming_and_complex_elements() {
    let mut app = create_test_app().await;
    let md_streaming = r#"> Note: this is a blockquote

| Column A | Column B |
| -------- | -------- |
| Value 1  | Value 2  |

```python
def stream_test():
pass
"#;
    app.add_part(
        Role::Assistant,
        ContentPart::Text {
            text: md_streaming.to_string(),
            annotations: Vec::new(),
        },
    );

    let lines = build_chat_lines(&app, 80);
    assert!(!lines.is_empty());

    let all_text = plain(&lines);
    assert!(all_text.contains("Note: this is a blockquote"));
    assert!(all_text.contains("Column A"));
    assert!(all_text.contains("Value 1"));
    assert!(all_text.contains("def stream_test():"));
}

#[tokio::test]
async fn test_render_chat_scrollbar_and_scrolling() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = create_test_app().await;
    // 添加大量文本使得行数超过视口（高度为 10）
    for i in 0..30 {
        app.add_part(Role::User, ContentPart::text(format!("Message line {i}")));
    }

    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            render_chat(&mut app, f, f.area());
        })
        .unwrap();

    // 验证视口和滚动状态已自动计算，并且处于底部
    let viewport = Rect::new(2, 2, 76, 7);
    assert!(app.max_scroll > 0);
    assert_eq!(app.viewport_height, 7);
    assert!(app.auto_scroll);
    assert_eq!(app.scroll_offset, app.max_scroll);
    assert_eq!(app.chat.area, viewport);

    // 验证滚到底时，滚动条滑块位于视口最底部行
    let bar_x = 79;
    assert_eq!(
        app.scrollbar_area,
        Rect::new(bar_x, viewport.y, 1, viewport.height)
    );
    let buffer = terminal.backend().buffer();
    let last_cell = buffer
        .cell((bar_x, viewport.y + viewport.height - 1))
        .unwrap();
    assert_eq!(last_cell.symbol(), SCROLLBAR_THUMB);
    assert_eq!(last_cell.fg, Color::Rgb(130, 140, 158));
    assert_eq!(buffer.cell((bar_x, viewport.y)).unwrap().fg, PANEL_BORDER);
    assert_eq!(buffer.cell((77, viewport.y)).unwrap().symbol(), " ");
    assert_eq!(buffer.cell((bar_x, 9)).unwrap().symbol(), "╯");

    // 滚到顶时，滚动条滑块位于视口最顶部行
    app.scroll_to_top();
    terminal
        .draw(|f| {
            render_chat(&mut app, f, f.area());
        })
        .unwrap();
    let top_cell = terminal
        .backend()
        .buffer()
        .cell((bar_x, viewport.y))
        .unwrap();
    assert_eq!(top_cell.symbol(), SCROLLBAR_THUMB);
    assert_eq!(top_cell.fg, Color::Rgb(130, 140, 158));
}

#[tokio::test]
async fn test_render_chat_selection_overlay() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = create_test_app().await;
    app.add_part(Role::User, ContentPart::text("hello world"));

    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            render_chat(&mut app, f, f.area());
        })
        .unwrap();

    let line_idx = app
        .chat
        .lines
        .iter()
        .position(|line| line.spans.iter().any(|span| span.content.contains("hello")))
        .expect("user message rendered");
    app.chat.selection.start(line_idx, 0);
    app.chat.selection.update(line_idx, 5);
    app.chat.selection.finish();

    terminal
        .draw(|f| {
            render_chat(&mut app, f, f.area());
        })
        .unwrap();

    let scroll = app.chat_scroll();
    let area = app.chat.area;
    let row = area.y + (line_idx.saturating_sub(scroll)) as u16;
    let cell = terminal.backend().buffer().cell((area.x, row)).unwrap();
    assert_eq!(cell.bg, USER_BLOCK_BG);
    let text_cell = terminal.backend().buffer().cell((area.x + 4, row)).unwrap();
    assert_eq!(text_cell.bg, Color::Rgb(60, 90, 140));
    let outside = terminal.backend().buffer().cell((area.x + 6, row)).unwrap();
    assert_ne!(outside.bg, Color::Rgb(60, 90, 140));
}

#[test]
fn test_highlight_offset_window_above_selection_start() {
    // 选区从窗口上方开始：窗口首行应整行高亮，而非从原起始列开始
    let mut window = vec![
        Line::from(vec![Span::raw("second")]),
        Line::from(vec![Span::raw("third")]),
    ];
    let sel = TextSelection::new(TextCoord { line: 0, col: 3 }, TextCoord { line: 2, col: 2 });
    apply_selection_highlight_offset(&mut window, &sel, 1);
    let first_line_selected: String = window[0]
        .spans
        .iter()
        .filter(|span| span.style.bg.is_some())
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(first_line_selected, "second");
}

#[test]
fn test_extract_selected_text_single_line() {
    let lines = vec![Line::from(vec![
        Span::raw("Hello "),
        Span::styled("World", Style::default().fg(Color::Green)),
        Span::raw("!"),
    ])];
    let sel = TextSelection::new(
        TextCoord { line: 0, col: 6 },
        TextCoord { line: 0, col: 11 },
    );
    let extracted = extract_selected_text(&lines, &sel);
    assert_eq!(extracted, Some("World".to_string()));
}

#[test]
fn test_extract_selected_text_multiline_cjk() {
    let lines = vec![
        Line::from(vec![Span::raw("第一行：你好世界")]),
        Line::from(vec![Span::raw("第二行：Rust编程语言")]),
    ];
    // 第一行 "第一行：" 宽度为 4*2=8 -> col 8 开始选取 "你好世界"
    // 第二行 "第二行：" 宽度为 8，"Rust" 宽度为 4 -> col 12 结束选取
    let sel = TextSelection::new(
        TextCoord { line: 0, col: 8 },
        TextCoord { line: 1, col: 12 },
    );
    let extracted = extract_selected_text(&lines, &sel);
    assert_eq!(extracted, Some("你好世界\n第二行：Rust".to_string()));
}

#[test]
fn test_extract_selected_text_reverse_drag() {
    let lines = vec![
        Line::from(vec![Span::raw("Line 0 alpha")]),
        Line::from(vec![Span::raw("Line 1 beta")]),
    ];
    let sel = TextSelection::new(
        TextCoord { line: 1, col: 11 },
        TextCoord { line: 0, col: 7 },
    );
    let extracted = extract_selected_text(&lines, &sel);
    assert_eq!(extracted, Some("alpha\nLine 1 beta".to_string()));
}

#[test]
fn test_apply_selection_highlight() {
    let mut lines = vec![Line::from(vec![
        Span::styled("Prefix ", Style::default().fg(Color::Yellow)),
        Span::styled("Selected", Style::default().fg(Color::Green)),
        Span::styled(" Suffix", Style::default().fg(Color::Red)),
    ])];
    let sel = TextSelection::new(
        TextCoord { line: 0, col: 7 },
        TextCoord { line: 0, col: 15 },
    );
    apply_selection_highlight(&mut lines, &sel);

    assert_eq!(lines[0].spans.len(), 3);
    assert_eq!(lines[0].spans[0].content, "Prefix ");
    assert_eq!(lines[0].spans[0].style.fg, Some(Color::Yellow));

    assert_eq!(lines[0].spans[1].content, "Selected");
    assert_eq!(lines[0].spans[1].style.bg, Some(Color::Rgb(60, 90, 140)));
    assert_eq!(lines[0].spans[1].style.fg, Some(Color::White));

    assert_eq!(lines[0].spans[2].content, " Suffix");
    assert_eq!(lines[0].spans[2].style.fg, Some(Color::Red));
}

#[test]
fn test_agents_reminders_rendering_visibility() {
    use crate::system::*;
    use std::path::Path;

    let mut sink = Sink::default();
    let mut append = |text: String| {
        sink.append(Role::User, std::iter::once(&ContentPart::text(text)));
        sink.text()
    };

    // 1. 全局与项目 AGENTS.md：不展示
    assert!(
        append(format_global_agents_reminder(
            Path::new("/home/user/.vva/AGENTS.md"),
            "global instructions",
        ))
        .is_empty()
    );
    assert!(
        append(format_project_agents_reminder(
            Path::new("/workspace/AGENTS.md"),
            "project instructions",
        ))
        .is_empty()
    );

    // 2. 按需自动加载的子目录 AGENTS.md：展示 "Load xxx/AGENTS.md"
    let output = append(format_directory_agents_reminder(
        Path::new("/workspace"),
        Path::new("/workspace/src/ui/AGENTS.md"),
        "ui instructions",
    ));
    assert!(output.contains("Load src/ui/AGENTS.md"));
    assert!(!output.contains("ui instructions"));

    // 3. /<skill_name> 直接加载的技能：仅展示 "Load <name>"，正文不外显
    let skill = crate::skills::Skill {
        name: "pdf-processing".into(),
        description: "Extract PDF text.".into(),
        license: None,
        compatibility: None,
        metadata: Default::default(),
        allowed_tools: None,
        path: std::path::PathBuf::from("/skills/pdf-processing"),
        source: crate::skills::SkillSource::Global,
        body: "1. Open the file.".into(),
    };
    let output = append(format_skill_load_reminder(&skill));
    assert!(output.contains("Load pdf-processing"));
    assert!(!output.contains("Open the file"));
    assert!(!output.contains("system-reminder"));

    // 技能清单 reminder 不展示
    let before = output;
    assert_eq!(append(format_skills_reminder(&[skill])), before);
}

#[test]
fn external_web_search_renders_native_row_with_sources() {
    let call = ToolCall::new("c1", "web_search", r#"{"query":"rust async"}"#);
    let result = ToolResult::success(
        "c1",
        "1. Tokio Tutorial\nhttps://tokio.rs/tutorial\nshared state\n\n2. https://example.org/plain\nplain page\n",
    );
    let mut results = HashMap::new();
    results.insert(result.tool_call_id.clone(), &result);
    let (lines, hits) = render_tools(
        &[ContentPart::ToolCall(call)],
        &results,
        &HashMap::new(),
        80,
    );
    let text = joined(&lines);

    // 原生形态：无箭头，label + 来源数，行下 title 居左域名居右的来源列表
    assert!(
        text.contains("web_search \"rust async\" (2 sources)"),
        "{text}"
    );
    assert!(text.contains("Tokio Tutorial"), "{text}");
    assert!(text.contains("tokio.rs"), "{text}");
    assert!(text.contains("example.org"), "{text}");
    // 不渲染工具输出内容块
    assert!(!text.contains("shared state"), "{text}");
    assert!(!text.contains("plain page"), "{text}");
    assert!(hits.is_empty());
}

#[test]
fn external_web_fetch_renders_native_open_page_row() {
    let call = ToolCall::new("c1", "web_fetch", r#"{"url":"https://example.com/post"}"#);
    let result = ToolResult::success("c1", "# Post\n\nlong page body\n".repeat(50));
    let mut results = HashMap::new();
    results.insert(result.tool_call_id.clone(), &result);
    let (lines, hits) = render_tools(
        &[ContentPart::ToolCall(call)],
        &results,
        &HashMap::new(),
        80,
    );
    let text = joined(&lines);

    assert!(
        text.contains("web_fetch https://example.com/post"),
        "{text}"
    );
    assert!(!text.contains("long page body"), "{text}");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn feedback_stays_outside_transcript_and_replaces_equal_length_content() {
    use crate::harness::{Notice, NoticeKind};
    let mut app = create_test_app().await;
    app.feedback.push("Local feedback".into());
    Arc::make_mut(&mut app.view).notices.push(Notice::new(
        "First capability".into(),
        NoticeKind::Capability,
    ));
    app.refresh_chat_lines(80);
    assert!(!joined(&app.chat.lines).contains("capability"));
    assert!(!joined(&app.chat.lines).contains("Local feedback"));
    let panel = crate::ui::views::input_area::feedback_lines(&app, 80);
    assert!(joined(&panel).contains("First capability"));
    Arc::make_mut(&mut app.view).notices[0].text = "Other capability".into();
    let panel = joined(&crate::ui::views::input_area::feedback_lines(&app, 80));
    assert!(panel.contains("Other capability"));
    assert!(!panel.contains("First capability"));
    Arc::make_mut(&mut app.view).notices.clear();
    assert_eq!(
        joined(&crate::ui::views::input_area::feedback_lines(&app, 80)).trim(),
        "Local feedback"
    );
}

#[tokio::test]
async fn turn_outcome_and_task_completion_render_at_their_owners() {
    use crate::harness::{Notice, NoticeKind};
    let mut app = create_test_app().await;
    let first = crate::model::Turn::user_turn(crate::model::Message::text("First request"));
    let first_id = first.id.clone();
    let mut result = ToolResult::success("call-build", "Started");
    result.background_task_id = Some("task-build".into());
    let call = ToolCall::new(
        "call-build",
        "Shell",
        r#"{"intent":"Build files","command":"build","run_in_background":true}"#,
    );
    let view = Arc::make_mut(&mut app.view);
    let session = Arc::make_mut(&mut view.session);
    session.turns = vec![
        first,
        crate::model::Turn::assistant_turn(vec![crate::model::Message::new(vec![
            ContentPart::ToolCall(call),
        ])]),
        crate::model::Turn::new(Role::Tool, vec![crate::model::Message::tool_result(result)]),
        crate::model::Turn::user_turn(crate::model::Message::text("Later request")),
    ];
    view.notices = vec![
        Notice::new(
            "Execution stopped".into(),
            NoticeKind::Turn {
                turn_id: first_id,
                aborted: true,
            },
        ),
        Notice::new(
            "Build completed".into(),
            NoticeKind::Task {
                task_id: "task-build".into(),
            },
        ),
    ];
    app.refresh_chat_lines(120);
    let text = joined(&app.chat.lines);
    assert!(text.find("Execution stopped").unwrap() < text.find("Later request").unwrap());
    let task_line = app
        .chat
        .lines
        .iter()
        .map(ToString::to_string)
        .find(|line| line.contains("Build completed"))
        .unwrap();
    assert!(task_line.contains("Shell"));
    assert!(text.find("Build completed").unwrap() < text.find("Later request").unwrap());
    Arc::make_mut(&mut app.view).notices[1].text = "Build failed".into();
    app.refresh_chat_lines(120);
    let text = joined(&app.chat.lines);
    assert!(text.contains("Build failed"));
    assert!(!text.contains("Build completed"));
}
