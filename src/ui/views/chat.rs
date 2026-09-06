use std::collections::HashSet;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::model::{Annotation, ContentPart, Role, WebSearchAction, WebSearchCall};
use crate::ui::app::{App, TextCoord, TextSelection, char_in_column_range};
use crate::ui::markdown::{render_markdown, wrap_spans_to_lines};
use crate::ui::text::truncate;

/// 聊天流正文可用宽度（右侧留出一列给滚动条）
pub fn chat_text_width(area: Rect) -> usize {
    (area.width as usize).saturating_sub(1)
}

/// 用户消息引用块的底色与左侧色条颜色
const USER_BLOCK_BG: Color = Color::Rgb(28, 32, 40);
const USER_BLOCK_BAR: Color = Color::Rgb(96, 149, 239);
const SELECTION_STYLE: Style = Style::new().fg(Color::White).bg(Color::Rgb(60, 90, 140));

/// 构建用户消息引用块的单行：左侧色条起头，块底色填充至可用宽度
fn user_block_row(spans: Vec<Span<'static>>, max_width: usize) -> Line<'static> {
    let bar_style = Style::default().fg(USER_BLOCK_BAR).bg(USER_BLOCK_BG);
    let base = Style::default().bg(USER_BLOCK_BG);
    let mut width = 1;
    let mut out = vec![Span::styled("▌", bar_style)];
    for span in spans {
        width += span.content.width();
        out.push(span);
    }
    if max_width > width {
        out.push(Span::styled(" ".repeat(max_width - width), base));
    }
    Line::from(out)
}

/// 以左侧色条引用块渲染用户消息（含上下留白行），与模型输出形成区块级区分
fn push_user_block(lines: &mut Vec<Line<'static>>, text: &str, max_width: usize) {
    let text_style = Style::default()
        .fg(Color::Rgb(226, 229, 234))
        .bg(USER_BLOCK_BG);
    let inner_width = max_width.saturating_sub(3);
    lines.push(user_block_row(Vec::new(), max_width));
    for l in text.lines() {
        let spans = vec![Span::styled(l.to_string(), text_style)];
        for line in wrap_spans_to_lines(&spans, inner_width, "   ", "   ") {
            let mut spans = line.spans;
            if let Some(indent) = spans.first_mut() {
                indent.style = text_style;
            }
            lines.push(user_block_row(spans, max_width));
        }
    }
    lines.push(user_block_row(Vec::new(), max_width));
}

/// 从 URL 提取展示用域名（去协议、端口与 www. 前缀）
fn url_display_domain(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.split(':').next().unwrap_or(host);
    host.strip_prefix("www.").unwrap_or(host)
}

/// Collect cited URLs in first-appearance order within a response.
fn search_citations<'a>(
    parts: impl IntoIterator<Item = &'a ContentPart>,
) -> Vec<(&'a str, &'a str)> {
    let mut seen = HashSet::new();
    let mut citations = Vec::new();
    for part in parts {
        if let ContentPart::Text { annotations, .. } = part {
            for a in annotations {
                if let Annotation::UrlCitation { url, title, .. } = a
                    && seen.insert(url.as_str())
                {
                    citations.push((url.as_str(), title.as_str()));
                }
            }
        }
    }
    citations
}

fn call_covers_url(call: &WebSearchCall, url: &str) -> bool {
    match &call.action {
        WebSearchAction::Search => call.sources.iter().any(|source| source.url == url),
        WebSearchAction::OpenPage { url: action_url }
        | WebSearchAction::FindInPage {
            url: action_url, ..
        } => action_url == url,
    }
}

/// 轮次内统一汇总引用；思考分组状态延续到下一轮或草稿。
pub(crate) fn append_parts<'a>(
    lines: &mut Vec<Line<'static>>,
    role: Role,
    parts: impl Iterator<Item = &'a ContentPart> + Clone,
    max_width: usize,
    thinking_concise: bool,
    prev_reasoning: &mut bool,
) {
    let calls = parts
        .clone()
        .filter_map(|part| match part {
            ContentPart::WebSearchCall(call) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    let citations = search_citations(parts.clone());
    for part in parts {
        match (role, part) {
            (Role::User, ContentPart::Text { text, .. }) => {
                *prev_reasoning = false;
                lines.push(Line::from(""));
                push_user_block(lines, text, max_width);
            }
            (Role::Assistant, ContentPart::Reasoning { content, .. }) => {
                // 连续思考段归为一段：仅组首插入空行分隔，组内紧密堆叠
                let group_start = !*prev_reasoning;
                *prev_reasoning = true;
                if group_start {
                    lines.push(Line::from(""));
                }
                // 各思考段落（协议层以空行分隔）独立渲染 markdown 并统一弱化为灰色斜体
                // （Style::patch 语义下 fg 覆盖、修饰符并集，加粗等强调得以保留）
                let body_style = Style::default()
                    .fg(Color::Rgb(120, 120, 120))
                    .add_modifier(Modifier::ITALIC);
                let prefix_style = Style::default()
                    .fg(Color::Rgb(140, 140, 140))
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC);

                if thinking_concise {
                    // hidden 模式：整组合并为单行标记，段落内容隐藏
                    if group_start {
                        lines.push(Line::from(vec![Span::styled("+ Thought:", prefix_style)]));
                    }
                } else {
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
            (Role::Assistant, ContentPart::ToolCall(call)) => {
                *prev_reasoning = false;
                let name = &call.name;
                let args = &call.arguments;
                lines.push(Line::from(""));
                let spans = vec![
                    Span::styled(
                        format!("[{}]", name),
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
            (Role::Tool, ContentPart::ToolResult(result)) => {
                *prev_reasoning = false;
                let is_error = &result.is_error;
                let result = &result.content;
                let status_color = if *is_error { Color::Red } else { Color::Green };
                let status_label = if *is_error { "Error:" } else { "Output:" };

                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        status_label,
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
            (Role::Assistant, ContentPart::WebSearchCall(call)) => {
                *prev_reasoning = false;
                let (label, detail) = match &call.action {
                    WebSearchAction::Search => (
                        "[web_search]",
                        format!("{} ({} sources)", call.query, call.sources.len()),
                    ),
                    WebSearchAction::OpenPage { url } => ("[web_open]", url.clone()),
                    WebSearchAction::FindInPage { url, pattern } => {
                        ("[web_find]", format!("{url} / {pattern}"))
                    }
                };
                lines.push(Line::from(""));
                let spans = vec![
                    Span::styled(
                        label,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(detail, Style::default().fg(Color::Rgb(160, 160, 160))),
                ];
                let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
                lines.extend(wrapped);

                if matches!(call.action, WebSearchAction::OpenPage { .. }) {
                    continue;
                }

                // Citation titles on the left, domains aligned to the right.
                for (url, title) in &citations {
                    if !call_covers_url(call, url) {
                        continue;
                    }
                    let domain = url_display_domain(url);
                    let title = title.trim();
                    let title = if title.is_empty() || title == *url {
                        domain
                    } else {
                        title
                    };
                    let domain = truncate(domain, max_width.saturating_sub(4) / 2);
                    let right_w = domain.width();
                    let title_max = max_width.saturating_sub(3 + right_w + 1);
                    let title = truncate(title, title_max);
                    let pad = title_max - title.width() + 1;
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(title, Style::default().fg(Color::Rgb(170, 170, 170))),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(domain.to_string(), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            (Role::Assistant, ContentPart::Text { text, annotations }) => {
                *prev_reasoning = false;
                let citations: Vec<_> = annotations
                    .iter()
                    .filter_map(|a| {
                        if let Annotation::UrlCitation { title, url, .. } = a
                            && !calls.iter().any(|call| call_covers_url(call, url))
                        {
                            Some((title, url))
                        } else {
                            None
                        }
                    })
                    .collect();
                lines.push(Line::from(""));
                let md_lines = render_markdown(text, max_width);
                lines.extend(md_lines);

                // Citations not covered by a call remain below the text.
                if !citations.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            "Sources / Citations:",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for (i, (title, url)) in citations.iter().enumerate() {
                        let spans = vec![
                            Span::styled(format!("[{}] ", i + 1), Style::default().fg(Color::Cyan)),
                            Span::styled((*title).clone(), Style::default().fg(Color::White)),
                            Span::raw(" - "),
                            Span::styled(
                                (*url).clone(),
                                Style::default().fg(Color::Rgb(100, 160, 255)),
                            ),
                        ];
                        let wrapped = wrap_spans_to_lines(&spans, max_width, "     ", "       ");
                        lines.extend(wrapped);
                    }
                }
            }
            // WebSearchCall 等未渲染的 part 不打断思考分组的连续性
            _ => {}
        }
    }
}

pub(crate) fn append_chat_tail(
    lines: &mut Vec<Line<'static>>,
    view: &crate::harness::SessionView,
    notices: &[String],
    max_width: usize,
    thinking_concise: bool,
    mut prev_reasoning: bool,
) {
    if view.session.turns.iter().all(|t| t.role == Role::System) && view.draft.is_empty() {
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
    append_parts(
        lines,
        Role::Assistant,
        view.draft.iter(),
        max_width,
        thinking_concise,
        &mut prev_reasoning,
    );
    for text in notices {
        lines.push(Line::from(""));
        let spans = vec![Span::styled(
            text.clone(),
            Style::default()
                .fg(Color::Rgb(200, 200, 200))
                .add_modifier(Modifier::ITALIC),
        )];
        let wrapped = wrap_spans_to_lines(&spans, max_width, "", "   ");
        lines.extend(wrapped);
    }
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
    apply_selection_highlight_offset(lines, selection, 0);
}

/// 同上，`offset` 为切片首行在整体行列表中的行号（渲染可视窗口时使用）
pub fn apply_selection_highlight_offset(
    lines: &mut [Line<'static>],
    selection: &TextSelection,
    offset: usize,
) {
    if selection.is_empty() {
        return;
    }
    let (start, end) = selection.normalized();
    if end.line < offset {
        return;
    }
    // 起点落在窗口上方时，窗口首行按整行选中处理
    let start = if start.line < offset {
        TextCoord { line: 0, col: 0 }
    } else {
        TextCoord {
            line: start.line - offset,
            col: start.col,
        }
    };
    let end = TextCoord {
        line: end.line - offset,
        col: end.col,
    };
    let selection = &TextSelection::new(start, end);
    let max_line = end.line.min(lines.len().saturating_sub(1));
    if start.line > max_line {
        return;
    }

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
                        let style = if sel { SELECTION_STYLE } else { span.style };
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
                let style = if sel { SELECTION_STYLE } else { span.style };
                new_spans.push(Span::styled(cur_buf, style));
            }
        }

        line.spans = new_spans;
    }
}

/// 在已绘制的可视行上覆盖选区底色，避免克隆行并重建 span
fn overlay_selection(
    buf: &mut Buffer,
    area: Rect,
    line_count: usize,
    selection: &TextSelection,
    scroll: usize,
) {
    if selection.is_empty() {
        return;
    }
    let text_width = area.width.saturating_sub(1) as usize;
    if text_width == 0 {
        return;
    }
    for i in 0..line_count {
        let Some((start_col, end_col)) = selection.line_column_range(scroll + i) else {
            continue;
        };
        let start_col = start_col.min(text_width);
        let end_col = end_col.min(text_width);
        if start_col >= end_col {
            continue;
        }
        buf.set_style(
            Rect {
                x: area.x + start_col as u16,
                y: area.y + i as u16,
                width: (end_col - start_col) as u16,
                height: 1,
            },
            SELECTION_STYLE,
        );
    }
}

/// 对话与工具消息流视图（支持 Thinking 思考块、Tool 动作与结果、Assistant 最终回复 Markdown 渲染）
pub fn render_chat(app: &mut App, frame: &mut Frame, area: Rect) {
    app.chat.area = area;

    app.refresh_chat_lines(chat_text_width(area));
    let total_lines = app.chat.lines.len();
    let viewport_height = area.height as usize;
    let max_scroll = total_lines.saturating_sub(viewport_height);

    app.update_viewport(max_scroll, viewport_height);

    let scroll = app.chat_scroll();
    let start = scroll.min(total_lines);
    let end = (scroll + viewport_height).min(total_lines);
    let selection = app.chat.selection.get().copied();
    let text_width = area.width.saturating_sub(1);

    {
        let window = &app.chat.lines[start..end];
        for (i, line) in window.iter().enumerate() {
            frame.render_widget(
                line,
                Rect {
                    x: area.x,
                    y: area.y + i as u16,
                    width: text_width,
                    height: 1,
                },
            );
        }
        if let Some(sel) = selection {
            overlay_selection(frame.buffer_mut(), area, window.len(), &sel, scroll);
        }
    }

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
    use crate::model::WebSearchSource;
    use crate::ui::app::TextCoord;
    use std::sync::Arc;

    fn build_chat_lines(app: &App, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut prev_reasoning = false;
        for turn in &app.view.session.turns {
            append_parts(
                &mut lines,
                turn.role,
                turn.messages.iter().flat_map(|m| &m.parts),
                width.max(20),
                app.thinking_concise,
                &mut prev_reasoning,
            );
        }
        append_chat_tail(
            &mut lines,
            &app.view,
            &app.notices,
            width.max(20),
            app.thinking_concise,
            prev_reasoning,
        );
        lines
    }

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
    async fn test_chat_cache_preserves_history_and_layout() {
        let mut ta =
            crate::ui::test_support::create_test_app("init_model", None, None, false).await;
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
            vec![ContentPart::reasoning("**Next thought**", None)],
            vec![
                ContentPart::reasoning("**Next thought**", None),
                ContentPart::text("**Answer**"),
            ],
            vec![],
        ] {
            let view = Arc::make_mut(&mut app.view);
            view.revision += 1;
            view.draft = draft;
            check(app, 80);
            assert_eq!(history_text(app), history);
        }
        app.notices.push("Local notice".into());
        check(app, 80);
        app.notices.clear();
        check(app, 80);
        assert_eq!(history_text(app), history);

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft
            .push(ContentPart::reasoning("**Next thought**", None));
        for concise in [false, true] {
            app.thinking_concise = concise;
            for width in [30, 120, 80] {
                check(app, width);
            }
        }
        // 会话内容替换时，即使轮次数与视图版本相同也必须重建历史。
        let session = Arc::make_mut(&mut Arc::make_mut(&mut app.view).session);
        session.turns[0] =
            crate::model::Turn::user_turn(crate::model::Message::text("Replacement"));
        check(app, 80);
        app.attach_session(app.active_session.clone());
        check(app, 80);
        assert!(
            app.chat
                .lines
                .iter()
                .any(|line| line.to_string().contains("Start typing"))
        );
        std::fs::remove_dir_all(ta.temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_thinking_grouping_and_concise_modes() {
        let mut app = create_test_app().await;
        app.thinking_concise = false;
        // 模拟协议层输出：各 summary part 带原始 markdown 标记，以空行（段落边界）分隔
        app.add_part(
            Role::Assistant,
            ContentPart::reasoning(
                "**Requesting Shenzhen weather forecast**\n\n**Drafting cautious forecast**"
                    .to_string(),
                None,
            ),
        );

        // full 模式：每个思考段落一条独立的 `+ Thought:` 行
        let lines = build_chat_lines(&app, 80);
        let thought_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.spans.first().is_some_and(|s| s.content == "+ Thought: "))
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
        let bold_in_first = thought_lines[0].spans.iter().any(|s| {
            s.content.contains("Requesting") && s.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(bold_in_first);

        // 追加搜索调用与第二个思考 part：搜索块渲染为分组边界，前后思考各自成段。
        // 搜索调用、思考与带注脚的正文同属一轮，来源由同轮注脚提供。
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        Arc::make_mut(&mut view.session).append_turn(crate::model::Turn::new(
            Role::Assistant,
            vec![
                crate::model::Message::new(vec![ContentPart::WebSearchCall(
                    WebSearchCall::completed(
                        "ws_1",
                        "深圳天气",
                        vec![WebSearchSource::new(
                            "https://weather.cma.cn/web/weather/59493.html",
                            "Uncited candidate",
                            None,
                        )],
                    ),
                )]),
                crate::model::Message::new(vec![ContentPart::reasoning(
                    "**Cross-checking sources**".to_string(),
                    None,
                )]),
                crate::model::Message::new(vec![ContentPart::Text {
                    text: "今天深圳多云转晴。".to_string(),
                    annotations: vec![Annotation::UrlCitation {
                        title: "深圳市气象局".into(),
                        url: "https://weather.cma.cn/web/weather/59493.html".into(),
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
            .filter(|(_, l)| l.spans.first().is_some_and(|s| s.content == "+ Thought: "))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(header_idx.len(), 3);

        // 同组思考行之间无空行
        for line in &lines[header_idx[0] + 1..header_idx[1]] {
            assert!(
                !line.spans.is_empty() && line.spans.iter().any(|s| !s.content.trim().is_empty())
            );
        }
        // 搜索块出现在两组思考之间，来源行左侧为注脚补齐的网页标题、域名右对齐至可用宽度
        let search_idx = lines
            .iter()
            .position(|l| l.spans.iter().any(|s| s.content == "[web_search]"))
            .expect("web search block rendered");
        assert!(search_idx > header_idx[1] && search_idx < header_idx[2]);
        let source_row = &lines[search_idx + 1];
        assert!(
            source_row
                .spans
                .iter()
                .any(|s| s.content.contains("深圳市气象局"))
        );
        let last = source_row.spans.last().unwrap();
        assert_eq!(last.content, "weather.cma.cn");
        let row_width: usize = source_row.spans.iter().map(|s| s.content.width()).sum();
        assert_eq!(row_width, 80);

        // 轮次内已有搜索调用：正文后的末尾来源汇总不再渲染
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("今天深圳多云转晴"));
        assert!(all_text.contains("深圳天气 (1 sources)"));
        assert!(!all_text.contains("candidate"));
        assert!(!all_text.contains("Sources / Citations"));

        // concise 模式：搜索块两侧的思考各自合并为单行标记，段落内容隐藏
        app.thinking_concise = true;
        let lines = build_chat_lines(&app, 80);
        let headers = lines
            .iter()
            .filter(|l| l.spans.first().is_some_and(|s| s.content == "+ Thought:"))
            .count();
        assert_eq!(headers, 2);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(!all_text.contains("Requesting Shenzhen weather forecast"));
        assert!(!all_text.contains("**"));
    }

    #[tokio::test]
    async fn test_separate_search_calls_track_draft_citations_and_turn_boundaries() {
        let mut app = create_test_app().await;
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft = vec![
            ContentPart::text("Before search"),
            ContentPart::WebSearchCall(WebSearchCall::completed(
                "ws_1",
                "query A; query A2",
                vec![
                    WebSearchSource::new(
                        "https://candidate.example/page",
                        "Uncited candidate",
                        None,
                    ),
                    WebSearchSource::new(
                        "https://www.cited.example/page",
                        "Source title must not appear",
                        None,
                    ),
                ],
            )),
            ContentPart::text("Between searches"),
            ContentPart::WebSearchCall(WebSearchCall::completed(
                "ws_2",
                "query B",
                vec![
                    WebSearchSource::new("https://www.cited.example/page", "Ignored", None),
                    WebSearchSource::new("https://other.example/page", "Ignored", None),
                ],
            )),
        ];
        app.refresh_chat_lines(100);
        let text = |lines: &[Line<'_>]| -> String {
            lines
                .iter()
                .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
                .collect()
        };
        let initial = text(&app.chat.lines);
        assert_eq!(initial.matches("[web_search]").count(), 2);
        assert!(initial.contains("[web_search] query A; query A2 (2 sources)"));
        assert!(initial.contains("[web_search] query B (2 sources)"));
        assert!(!initial.contains("candidate"));
        assert!(initial.find("Before search").unwrap() < initial.find("[web_search]").unwrap());
        assert!(initial.find("[web_search]").unwrap() < initial.find("Between searches").unwrap());
        assert!(initial.find("Between searches").unwrap() < initial.find("query B").unwrap());

        let citation = Annotation::UrlCitation {
            url: "https://www.cited.example/page".into(),
            title: "Provided title".into(),
            start_index: 0,
            end_index: 1,
        };
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft.push(ContentPart::Text {
            text: "Answer".into(),
            annotations: vec![citation.clone()],
        });
        app.refresh_chat_lines(100);
        let updated = text(&app.chat.lines);
        assert!(updated.contains("query A; query A2 (2 sources)"));
        assert!(updated.contains("query B (2 sources)"));
        assert_eq!(updated.matches("Provided title").count(), 2);
        assert!(updated.contains("cited.example"));
        assert!(!updated.contains("candidate"));

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft.push(ContentPart::Text {
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
        });
        app.refresh_chat_lines(100);
        let rendered = text(&app.chat.lines);
        assert_eq!(rendered.matches("[web_search]").count(), 2);
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
        let listing = rendered.split("Sources / Citations:").nth(1).unwrap();
        assert!(listing.contains("Unmatched reference - https://unmatched.example/page"));
        assert!(!listing.contains("Provided title"));
        assert!(!listing.contains("other.example"));

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        let messages = std::mem::take(&mut view.draft)
            .into_iter()
            .map(|part| crate::model::Message::new(vec![part]))
            .collect();
        Arc::make_mut(&mut view.session)
            .append_turn(crate::model::Turn::new(Role::Assistant, messages));
        app.refresh_chat_lines(100);
        assert_eq!(text(&app.chat.lines), rendered);

        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft
            .push(ContentPart::WebSearchCall(WebSearchCall::completed(
                "ws_3",
                "query C",
                vec![WebSearchSource::new(
                    "https://www.cited.example/page",
                    "Ignored",
                    None,
                )],
            )));
        app.refresh_chat_lines(100);
        let separate = text(&app.chat.lines);
        assert_eq!(separate.matches("[web_search]").count(), 3);
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
        open.action = WebSearchAction::OpenPage { url: url.into() };
        let mut find = WebSearchCall::completed("find", "unused query", vec![]);
        find.action = WebSearchAction::FindInPage {
            url: url.into(),
            pattern: "matching words".into(),
        };
        let view = Arc::make_mut(&mut app.view);
        view.revision += 1;
        view.draft = vec![
            ContentPart::WebSearchCall(open),
            ContentPart::text("Between actions"),
            ContentPart::WebSearchCall(find),
            ContentPart::Text {
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
            },
        ];
        let lines = build_chat_lines(&app, 100);
        let text: String = lines.iter().map(|line| line.to_string()).collect();
        assert!(text.contains(&format!("[web_open] {url}")));
        assert!(text.contains(&format!("[web_find] {url} / matching words")));
        assert!(!text.contains("sources)"));
        assert!(!text.contains("unused query"));
        assert_eq!(text.matches("Annotation title").count(), 1);
        let open_group = text.split("Between actions").next().unwrap();
        assert!(!open_group.contains("Annotation title"));
        assert!(!open_group.contains("Unmatched title"));
        let find_group = text
            .split("[web_find]")
            .nth(1)
            .unwrap()
            .split("Answer")
            .next()
            .unwrap();
        assert!(find_group.contains("Annotation title"));
        assert!(!find_group.contains("Unmatched title"));
        let listing = text.split("Sources / Citations:").nth(1).unwrap();
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
        let parts = std::mem::take(&mut view.draft);
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

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());

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
        app.add_part(
            Role::Assistant,
            ContentPart::Text {
                text: md_streaming.to_string(),
                annotations: Vec::new(),
            },
        );

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
        app.add_part(
            Role::Assistant,
            ContentPart::Text {
                text: String::new(),
                annotations: Vec::new(),
            },
        );

        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty());
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
        assert!(app.max_scroll > 0);
        assert_eq!(app.viewport_height, 10);
        assert!(app.auto_scroll);
        assert_eq!(app.scroll_offset, app.max_scroll);
        assert_eq!(app.chat.area, Rect::new(0, 0, 80, 10));

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
        let row = (line_idx.saturating_sub(scroll)) as u16;
        let cell = terminal.backend().buffer().cell((0, row)).unwrap();
        assert_eq!(cell.bg, Color::Rgb(60, 90, 140));
        let outside = terminal.backend().buffer().cell((6, row)).unwrap();
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
}
