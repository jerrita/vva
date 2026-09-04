use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::UnicodeWidthChar;

use crate::ui::app::{App, ChatEntry, TextSelection, char_in_column_range};
use crate::ui::markdown::{render_markdown, wrap_spans_to_lines};

/// 构建对话流的 Ratatui 文本行列表（支持 Markdown 渲染、CJK 安全折行与多级缩进）
pub fn build_chat_lines(app: &App, max_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let max_width = max_width.max(20);

    if app.chat_entries.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  Start typing a prompt or use ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "/",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to access commands (/sessions to resume).",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    for entry in &app.chat_entries {
        match entry {
            ChatEntry::UserPrompt { text } => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    "👤 You",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]));
                for l in text.lines() {
                    let spans = vec![Span::styled(
                        l.to_string(),
                        Style::default().fg(Color::White),
                    )];
                    let wrapped = wrap_spans_to_lines(&spans, max_width, "   ", "   ");
                    lines.extend(wrapped);
                }
            }
            ChatEntry::Thinking { content } => {
                // 各思考段落（协议层以空行分隔）独立渲染 markdown 并统一弱化为灰色斜体
                // （Style::patch 语义下 fg 覆盖、修饰符并集，加粗等强调得以保留）
                let body_style = Style::default()
                    .fg(Color::Rgb(120, 120, 120))
                    .add_modifier(Modifier::ITALIC);
                let prefix_style = Style::default()
                    .fg(Color::Rgb(140, 140, 140))
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC);
                // "+ Thought: " 前缀占 11 列，渲染时预留
                let paragraphs: Vec<Vec<Line<'static>>> = content
                    .split("\n\n")
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| {
                        let mut ls = render_markdown(p, max_width.saturating_sub(11));
                        for line in &mut ls {
                            for span in &mut line.spans {
                                span.style = span.style.patch(body_style);
                            }
                        }
                        ls
                    })
                    .collect();

                lines.push(Line::from(""));
                if app.thinking_concise {
                    let mut spans = vec![Span::styled("🧠 Thinking: ", prefix_style)];
                    match paragraphs.first().and_then(|ls| ls.first()) {
                        Some(first) => spans.extend(first.spans.iter().cloned()),
                        None => spans.push(Span::styled("Thinking...", body_style)),
                    }
                    lines.push(Line::from(spans));
                } else {
                    // OpenCode 风格：每个思考段落一条独立的 `+ Thought:` 行
                    let mut any = false;
                    for para in &paragraphs {
                        let mut iter = para.iter();
                        if let Some(first) = iter.next() {
                            let mut spans = vec![Span::styled("+ Thought: ", prefix_style)];
                            spans.extend(first.spans.iter().cloned());
                            lines.push(Line::from(spans));
                            lines.extend(iter.cloned());
                            any = true;
                        }
                    }
                    if !any {
                        lines.push(Line::from(vec![Span::styled("Thinking...", body_style)]));
                    }
                }
            }
            ChatEntry::ToolCall { name, args } => {
                lines.push(Line::from(""));
                let spans = vec![
                    Span::styled(
                        format!("⚙️ [{}]", name),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(args.clone(), Style::default().fg(Color::Rgb(160, 160, 160))),
                ];
                let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
                lines.extend(wrapped);
            }
            ChatEntry::ToolResult {
                name: _,
                result,
                is_error,
            } => {
                let status_color = if *is_error { Color::Red } else { Color::Green };
                let status_icon = if *is_error {
                    "❌ Error:"
                } else {
                    "✅ Output:"
                };

                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        status_icon,
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));

                let tool_lines: Vec<&str> = result.lines().collect();
                let limit = 20;
                for l in tool_lines.iter().take(limit) {
                    let spans = vec![Span::styled(
                        l.to_string(),
                        Style::default().fg(Color::Rgb(170, 170, 170)),
                    )];
                    let wrapped = wrap_spans_to_lines(&spans, max_width, "     ", "     ");
                    lines.extend(wrapped);
                }
                if tool_lines.len() > limit {
                    lines.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled(
                            format!("... ({} more lines hidden)", tool_lines.len() - limit),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }
            ChatEntry::AssistantMessage { text, citations } => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    "🤖 Assistant",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )]));
                let md_lines = render_markdown(text, max_width);
                lines.extend(md_lines);

                if !citations.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            "🔗 Sources / Citations:",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for (i, (title, url)) in citations.iter().enumerate() {
                        let spans = vec![
                            Span::styled(format!("[{}] ", i + 1), Style::default().fg(Color::Cyan)),
                            Span::styled(title.clone(), Style::default().fg(Color::White)),
                            Span::raw(" - "),
                            Span::styled(
                                url.clone(),
                                Style::default().fg(Color::Rgb(100, 160, 255)),
                            ),
                        ];
                        let wrapped = wrap_spans_to_lines(&spans, max_width, "     ", "       ");
                        lines.extend(wrapped);
                    }
                }
            }
            ChatEntry::SystemNotice { text } => {
                lines.push(Line::from(""));
                let spans = vec![
                    Span::styled("💡 ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        text.clone(),
                        Style::default()
                            .fg(Color::Rgb(200, 200, 200))
                            .add_modifier(Modifier::ITALIC),
                    ),
                ];
                let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
                lines.extend(wrapped);
            }
        }
    }

    lines
}

/// 从渲染完成的文本行中提取处于选区范围内的纯文本内容（按行保留视觉对齐并去除尾随空白）
pub fn extract_selected_text(lines: &[Line], selection: &TextSelection) -> Option<String> {
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.normalized();
    if start.line >= lines.len() {
        return None;
    }

    let mut result_lines = Vec::new();

    let end_idx = end.line.min(lines.len().saturating_sub(1));
    for (line_idx, line) in lines.iter().enumerate().take(end_idx + 1).skip(start.line) {
        let Some(range) = selection.line_column_range(line_idx) else {
            result_lines.push(String::new());
            continue;
        };

        let mut line_text = String::new();
        let mut col = 0;

        for span in &line.spans {
            for ch in span.content.chars() {
                if char_in_column_range(ch, col, range) {
                    line_text.push(ch);
                }
                col += ch.width().unwrap_or(0);
            }
        }

        result_lines.push(line_text.trim_end().to_string());
    }

    if result_lines.is_empty() {
        return None;
    }

    while let Some(last) = result_lines.last() {
        if last.is_empty() && result_lines.len() > 1 {
            result_lines.pop();
        } else {
            break;
        }
    }

    Some(result_lines.join("\n"))
}

/// 对处于选区范围内的文本行注入高亮样式（支持 CJK 字符边界精确切分）
pub fn apply_selection_highlight(lines: &mut [Line<'static>], selection: &TextSelection) {
    if selection.is_empty() {
        return;
    }
    let (start, end) = selection.normalized();
    let max_line = end.line.min(lines.len().saturating_sub(1));
    if start.line > max_line {
        return;
    }

    let highlight_style = Style::default()
        .bg(Color::Rgb(60, 90, 140))
        .fg(Color::White);

    for (line_idx, line) in lines
        .iter_mut()
        .enumerate()
        .take(max_line + 1)
        .skip(start.line)
    {
        let Some(range) = selection.line_column_range(line_idx) else {
            continue;
        };

        let old_spans = std::mem::take(&mut line.spans);
        let mut new_spans: Vec<Span<'static>> = Vec::new();
        let mut col = 0;

        for span in old_spans {
            let mut cur_buf = String::new();
            let mut cur_is_selected: Option<bool> = None;

            for ch in span.content.chars() {
                let is_selected = char_in_column_range(ch, col, range);

                match cur_is_selected {
                    Some(sel) if sel == is_selected => {
                        cur_buf.push(ch);
                    }
                    Some(sel) => {
                        let style = if sel { highlight_style } else { span.style };
                        new_spans.push(Span::styled(std::mem::take(&mut cur_buf), style));
                        cur_buf.push(ch);
                        cur_is_selected = Some(is_selected);
                    }
                    None => {
                        cur_buf.push(ch);
                        cur_is_selected = Some(is_selected);
                    }
                }
                col += ch.width().unwrap_or(0);
            }

            if let Some(sel) = cur_is_selected
                && !cur_buf.is_empty()
            {
                let style = if sel { highlight_style } else { span.style };
                new_spans.push(Span::styled(cur_buf, style));
            }
        }

        line.spans = new_spans;
    }
}

/// 对话与工具消息流视图（支持 Thinking 思考块、Tool 动作与结果、Assistant 最终回复 Markdown 渲染）
pub fn render_chat(app: &mut App, frame: &mut Frame, area: Rect) {
    app.chat_area = area;
    let block = Block::default().borders(Borders::NONE);

    let max_width = (area.width as usize).saturating_sub(1);
    let mut lines = build_chat_lines(app, max_width);
    let total_lines = lines.len();
    let viewport_height = area.height as usize;
    let max_scroll = total_lines.saturating_sub(viewport_height);

    app.update_viewport(max_scroll, viewport_height);

    let scroll = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    if let Some(sel) = app.text_selection.get() {
        apply_selection_highlight(&mut lines, sel);
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, area);

    // 当内容超出可视区时，在右侧渲染滚动条
    if max_scroll > 0 {
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll);

        let thumb_color = if app.is_dragging_scrollbar {
            Color::Rgb(150, 180, 225)
        } else {
            Color::Rgb(100, 110, 130)
        };

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(Style::default().fg(thumb_color))
            .track_style(Style::default().fg(Color::Rgb(35, 38, 46)));

        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::TextCoord;

    async fn create_test_app() -> App {
        crate::ui::test_support::create_test_app("init_model", None, None, false)
            .await
            .app
    }

    #[tokio::test]
    async fn test_chat_empty_placeholder() {
        let app = create_test_app().await;
        let lines = build_chat_lines(&app, 80);
        assert!(lines.len() >= 2);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(joined.contains("Start typing a prompt"));
    }

    #[tokio::test]
    async fn test_thinking_renders_one_thought_line_per_part() {
        let mut app = create_test_app().await;
        // 模拟协议层输出：各 summary part 带原始 markdown 标记，以空行（段落边界）分隔
        app.chat_entries.push(ChatEntry::Thinking {
            content: "**Requesting Shenzhen weather forecast**\n\n**Drafting cautious forecast**"
                .to_string(),
        });

        // full 模式：每个思考段落一条独立的 `+ Thought:` 行
        let lines = build_chat_lines(&app, 80);
        let thought_lines: Vec<_> = lines
            .iter()
            .filter(|l| {
                l.spans
                    .first()
                    .is_some_and(|s| s.content == "+ Thought: ")
            })
            .collect();
        assert_eq!(thought_lines.len(), 2);

        // markdown 标记被渲染掉，加粗修饰保留，两段分行
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Requesting Shenzhen weather forecast"));
        assert!(all_text.contains("Drafting cautious forecast"));
        assert!(!all_text.contains("**"));
        let bold_in_first = thought_lines[0]
            .spans
            .iter()
            .any(|s| s.content.contains("Requesting") && s.style.add_modifier.contains(Modifier::BOLD));
        assert!(bold_in_first);

        // concise 模式：单行预览展示首个思考段落，同样不含 markdown 标记
        app.thinking_concise = true;
        let lines = build_chat_lines(&app, 80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Thinking: "));
        assert!(all_text.contains("Requesting Shenzhen weather forecast"));
        assert!(!all_text.contains("**"));
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
        app.chat_entries.push(ChatEntry::AssistantMessage {
            text: md_content.to_string(),
            citations: vec![("Rust Lang".to_string(), "https://rust-lang.org".to_string())],
        });

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());

        // 检查 Assistant 标头存在
        let has_assistant_header = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("Assistant")));
        assert!(has_assistant_header);

        // 检查渲染出的 Markdown 内容
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Hello World"));
        assert!(all_text.contains("bold"));
        assert!(all_text.contains("inline code"));
        assert!(all_text.contains("Item 1"));
        assert!(all_text.contains("Item 2"));
        assert!(all_text.contains("println!"));
        assert!(all_text.contains("Rust Lang"));
        assert!(all_text.contains("https://rust-lang.org"));

        // 检查非空内容行均被添加了前缀缩进
        let assistant_lines: Vec<_> = lines
            .iter()
            .filter(|l| {
                l.spans
                    .iter()
                    .any(|s| s.content.contains("Hello World") || s.content.contains("bold"))
            })
            .collect();
        for line in assistant_lines {
            assert_eq!(line.spans.first().map(|s| s.content.as_ref()), Some("   "));
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
        app.chat_entries.push(ChatEntry::AssistantMessage {
            text: md_streaming.to_string(),
            citations: Vec::new(),
        });

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Note: this is a blockquote"));
        assert!(all_text.contains("Column A"));
        assert!(all_text.contains("Value 1"));
        assert!(all_text.contains("def stream_test():"));
    }

    #[tokio::test]
    async fn test_assistant_markdown_empty() {
        let mut app = create_test_app().await;
        app.chat_entries.push(ChatEntry::AssistantMessage {
            text: String::new(),
            citations: Vec::new(),
        });

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());
        let has_assistant = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("Assistant")));
        assert!(has_assistant);
    }

    #[tokio::test]
    async fn test_render_chat_scrollbar_and_scrolling() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = create_test_app().await;
        // 添加大量文本使得行数超过视口（高度为 10）
        for i in 0..30 {
            app.chat_entries.push(ChatEntry::UserPrompt {
                text: format!("Message line {i}"),
            });
        }

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_chat(&mut app, f, f.area());
            })
            .unwrap();

        // 验证视口和滚动状态已自动计算，并且处于底部
        assert!(app.max_scroll > 0);
        assert_eq!(app.viewport_height, 10);
        assert!(app.auto_scroll);
        assert_eq!(app.scroll_offset, app.max_scroll);
        assert_eq!(app.chat_area, Rect::new(0, 0, 80, 10));

        // 验证滚到底时，滚动条滑块位于最底部行 (y=9)
        let buffer = terminal.backend().buffer();
        let last_cell = buffer.cell((79, 9)).unwrap();
        assert_eq!(last_cell.symbol(), "█");

        // 滚到顶时，滚动条滑块位于最顶部行 (y=0)
        app.scroll_to_top();
        terminal
            .draw(|f| {
                render_chat(&mut app, f, f.area());
            })
            .unwrap();
        let top_cell = terminal.backend().buffer().cell((79, 0)).unwrap();
        assert_eq!(top_cell.symbol(), "█");
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
}
