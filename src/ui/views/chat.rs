use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::app::{App, ChatEntry};

/// 对话与工具消息流视图（支持 Thinking 思考块、Tool 动作与结果、Assistant 最终回复）
pub fn render_chat(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::NONE);

    let mut lines: Vec<Line> = Vec::new();

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
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(l, Style::default().fg(Color::White)),
                    ]));
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
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                l,
                                Style::default()
                                    .fg(Color::Rgb(120, 120, 120))
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                }
            }
            ChatEntry::ToolCall { name, args } => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("⚙️ [{}]", name),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        args,
                        Style::default().fg(Color::Rgb(160, 160, 160)),
                    ),
                ]));
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

                // 截取工具输出前 20 行以保持终端整洁
                let tool_lines: Vec<&str> = result.lines().collect();
                let limit = 20;
                for l in tool_lines.iter().take(limit) {
                    lines.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled(*l, Style::default().fg(Color::Rgb(170, 170, 170))),
                    ]));
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
                for l in text.lines() {
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(l, Style::default().fg(Color::White)),
                    ]));
                }
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
                        lines.push(Line::from(vec![
                            Span::raw("     "),
                            Span::styled(format!("[{}] ", i + 1), Style::default().fg(Color::Cyan)),
                            Span::styled(title, Style::default().fg(Color::White)),
                            Span::raw(" - "),
                            Span::styled(url, Style::default().fg(Color::Rgb(100, 160, 255))),
                        ]));
                    }
                }
            }
            ChatEntry::SystemNotice { text } => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("💡 ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        text,
                        Style::default()
                            .fg(Color::Rgb(200, 200, 200))
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }
    }

    let total_lines = lines.len();
    let viewport_height = area.height as usize;

    let scroll = if app.auto_scroll {
        if total_lines > viewport_height {
            (total_lines - viewport_height) as u16
        } else {
            0
        }
    } else {
        if total_lines > viewport_height {
            let max_scroll = total_lines - viewport_height;
            let offset = app.scroll_offset.min(max_scroll);
            (max_scroll - offset) as u16
        } else {
            0
        }
    };

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

