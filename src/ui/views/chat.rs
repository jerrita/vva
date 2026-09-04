use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::Frame;

use crate::ui::app::{App, ChatEntry};
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
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
                lines.push(Line::from(vec![
                    Span::styled(
                        "👤 You",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                for l in text.lines() {
                    let spans = vec![Span::styled(l.to_string(), Style::default().fg(Color::White))];
                    let wrapped = wrap_spans_to_lines(&spans, max_width, "   ", "   ");
                    lines.extend(wrapped);
                }
            }
            ChatEntry::Thinking { content } => {
                lines.push(Line::from(""));
                if app.thinking_concise {
                    let first_line = content
                        .lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("Thinking...");
                    let short_preview = if first_line.chars().count() > 80 {
                        format!("{}...", first_line.chars().take(77).collect::<String>())
                    } else {
                        first_line.to_string()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            "🧠 Thinking: ",
                            Style::default()
                                .fg(Color::Rgb(140, 140, 140))
                                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
                        ),
                        Span::styled(
                            short_preview,
                            Style::default()
                                .fg(Color::Rgb(120, 120, 120))
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "🧠 Thinking",
                            Style::default()
                                .fg(Color::Rgb(140, 140, 140))
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                    for l in content.lines() {
                        let spans = vec![Span::styled(
                            l.to_string(),
                            Style::default()
                                .fg(Color::Rgb(120, 120, 120))
                                .add_modifier(Modifier::ITALIC),
                        )];
                        let wrapped = wrap_spans_to_lines(&spans, max_width, "   ", "   ");
                        lines.extend(wrapped);
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
                    Span::styled(
                        args.clone(),
                        Style::default().fg(Color::Rgb(160, 160, 160)),
                    ),
                ];
                let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
                lines.extend(wrapped);
            }
            ChatEntry::ToolResult { name: _, result, is_error } => {
                let status_color = if *is_error { Color::Red } else { Color::Green };
                let status_icon = if *is_error { "❌ Error:" } else { "✅ Output:" };

                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        status_icon,
                        Style::default().fg(status_color).add_modifier(Modifier::BOLD),
                    ),
                ]));

                let tool_lines: Vec<&str> = result.lines().collect();
                let limit = 20;
                for l in tool_lines.iter().take(limit) {
                    let spans = vec![Span::styled(l.to_string(), Style::default().fg(Color::Rgb(170, 170, 170)))];
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
                lines.push(Line::from(vec![
                    Span::styled(
                        "🤖 Assistant",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                let md_lines = render_markdown(text, max_width);
                lines.extend(md_lines);

                if !citations.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            "🔗 Sources / Citations:",
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for (i, (title, url)) in citations.iter().enumerate() {
                        let spans = vec![
                            Span::styled(format!("[{}] ", i + 1), Style::default().fg(Color::Cyan)),
                            Span::styled(title.clone(), Style::default().fg(Color::White)),
                            Span::raw(" - "),
                            Span::styled(url.clone(), Style::default().fg(Color::Rgb(100, 160, 255))),
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

/// 对话与工具消息流视图（支持 Thinking 思考块、Tool 动作与结果、Assistant 最终回复 Markdown 渲染）
pub fn render_chat(app: &mut App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::NONE);

    let max_width = (area.width as usize).saturating_sub(1);
    let lines = build_chat_lines(app, max_width);
    let total_lines = lines.len();
    let viewport_height = area.height as usize;
    let max_scroll = total_lines.saturating_sub(viewport_height);

    app.update_viewport(max_scroll, viewport_height);

    let scroll = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, area);

    // 当内容超出可视区时，在右侧渲染半透明优雅的滚动条
    if max_scroll > 0 {
        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .position(scroll)
            .viewport_content_length(viewport_height);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(Style::default().fg(Color::Rgb(100, 110, 130)))
            .track_style(Style::default().fg(Color::Rgb(35, 38, 46)));

        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::jsonl::JsonlSessionStore;
    use crate::ui::session_manager::SessionManager;
    use std::sync::Arc;

    async fn create_test_app() -> App {
        let temp_dir = std::env::temp_dir().join(format!("vva_chat_test_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let store = Arc::new(JsonlSessionStore::new(&temp_dir).unwrap());
        let session_mgr = Arc::new(SessionManager::new(store, None, temp_dir.clone()));
        let session = session_mgr
            .create_session("init_model".into(), None, None)
            .await
            .unwrap();
        App::new(session_mgr, session, false).await
    }

    #[tokio::test]
    async fn test_chat_empty_placeholder() {
        let app = create_test_app().await;
        let lines = build_chat_lines(&app, 80);
        assert!(lines.len() >= 2);
        let joined: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(joined.contains("Start typing a prompt"));
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
        let has_assistant_header = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content.contains("Assistant"))
        });
        assert!(has_assistant_header);

        // 检查渲染出的 Markdown 内容
        let all_text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(all_text.contains("Hello World"));
        assert!(all_text.contains("bold"));
        assert!(all_text.contains("inline code"));
        assert!(all_text.contains("Item 1"));
        assert!(all_text.contains("Item 2"));
        assert!(all_text.contains("println!"));
        assert!(all_text.contains("Rust Lang"));
        assert!(all_text.contains("https://rust-lang.org"));

        // 检查非空内容行均被添加了前缀缩进
        let assistant_lines: Vec<_> = lines.iter().filter(|l| {
            l.spans.iter().any(|s| s.content.contains("Hello World") || s.content.contains("bold"))
        }).collect();
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

        let all_text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
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
        let has_assistant = lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("Assistant")));
        assert!(has_assistant);
    }

    #[tokio::test]
    async fn test_render_chat_scrollbar_and_scrolling() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = create_test_app().await;
        // 添加大量文本使得行数超过视口（高度为 10）
        for i in 0..30 {
            app.chat_entries.push(ChatEntry::UserPrompt {
                text: format!("Message line {i}"),
            });
        }

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            render_chat(&mut app, f, f.area());
        }).unwrap();

        // 验证视口和滚动状态已自动计算，并且 max_scroll > 0
        assert!(app.max_scroll > 0);
        assert_eq!(app.viewport_height, 10);
        assert!(app.auto_scroll);
        assert_eq!(app.scroll_offset, app.max_scroll);
    }
}

