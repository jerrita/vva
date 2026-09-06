use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};

use crate::ui::app::App;
use crate::ui::command::command_row;
use crate::ui::views::modal_shell::selected_row_style;
use crate::ui::views::{PANEL_BORDER, panel_block};

/// 复合输入区：圆角输入框，状态与元信息在框下方
pub fn render_input_area(
    app: &mut App,
    frame: &mut Frame,
    area: Rect,
    model: &str,
    variant: Option<&str>,
    workspace: &str,
    status_text: &str,
) {
    let is_running = app.active_session.is_running();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let block = panel_block()
        .border_style(Style::default().fg(if is_running {
            Color::Yellow
        } else {
            PANEL_BORDER
        }))
        .padding(Padding::horizontal(1));
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);

    let agent_status_spans = if is_running {
        let text = if status_text.is_empty() {
            "Working..."
        } else {
            status_text
        };
        vec![
            Span::styled(
                format!(" {} ", app.spinner_char()),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("Agent: {}", text),
                Style::default().fg(Color::Yellow),
            ),
        ]
    } else {
        vec![
            Span::styled(" ● ", Style::default().fg(Color::DarkGray)),
            Span::styled("Idle", Style::default().fg(Color::DarkGray)),
        ]
    };

    let web_search_span = if app.view.session.web_search {
        Span::styled(
            "🌐 Web Search: Enabled  ",
            Style::default().fg(Color::Green),
        )
    } else {
        Span::styled("🌐 Web Search: Off  ", Style::default().fg(Color::DarkGray))
    };

    let mut status_spans = agent_status_spans;
    status_spans.push(Span::raw("    "));
    status_spans.push(web_search_span);

    let text = inner;
    let inner_width = text.width as usize;
    let inner_height = text.height as usize;
    app.input_inner_area = text;
    let (cursor_row, cursor_col) = app.input.cursor_line_and_col();

    // 视口平滑滚动计算
    let mut scroll_y = app.input.scroll_y.get();
    if cursor_row < scroll_y {
        scroll_y = cursor_row;
    } else if inner_height > 0 && cursor_row >= scroll_y + inner_height {
        scroll_y = cursor_row + 1 - inner_height;
    }
    app.input.scroll_y.set(scroll_y);

    let mut scroll_x = app.input.scroll_x.get();
    if cursor_col < scroll_x {
        scroll_x = cursor_col;
    } else if inner_width > 0 && cursor_col >= scroll_x + inner_width {
        scroll_x = cursor_col + 1 - inner_width;
    }
    app.input.scroll_x.set(scroll_x);

    let input_p = if app.input.is_empty() {
        Paragraph::new(Line::from(vec![Span::styled(
            "Type a prompt or / for commands... (Enter to send, Ctrl+Enter for newline)",
            Style::default().fg(Color::Rgb(100, 100, 105)),
        )]))
    } else if let Some((sel_start, sel_end)) = app.input.selection_range() {
        let text_lines: Vec<&str> = app.input.text.split('\n').collect();
        let sel_style = Style::default()
            .bg(Color::Rgb(45, 80, 140))
            .fg(Color::White);
        let normal_style = Style::default().fg(Color::White);

        let mut rendered_lines = Vec::with_capacity(text_lines.len());
        let mut char_offset = 0;
        for line_str in text_lines {
            rendered_lines.push(Line::from(line_selection_spans(
                line_str,
                char_offset,
                (sel_start, sel_end),
                normal_style,
                sel_style,
            )));
            char_offset += line_str.chars().count() + 1; // +1 对应换行符 '\n'
        }

        Paragraph::new(ratatui::text::Text::from(rendered_lines))
            .scroll((scroll_y as u16, scroll_x as u16))
    } else {
        Paragraph::new(app.input.as_str()).scroll((scroll_y as u16, scroll_x as u16))
    };

    frame.render_widget(input_p, text);

    // 绘制光标 (基于行号和列显示宽度定位，正确支持多行、视口滚动及全角 CJK 字符)
    if !is_running
        && inner_height > 0
        && inner_width > 0
        && cursor_row >= scroll_y
        && cursor_row < scroll_y + inner_height
    {
        let row_offset = (cursor_row - scroll_y) as u16;
        let col_offset = cursor_col.saturating_sub(scroll_x) as u16;
        let cursor_x = text.x + col_offset;
        let cursor_y = text.y + row_offset;
        if cursor_x < text.x + text.width && cursor_y < text.y + text.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    let ws_name = std::path::Path::new(workspace)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(workspace);
    let variant_str = variant.unwrap_or("default");
    status_spans.push(Span::raw("    "));
    status_spans.push(Span::styled("🤖 ", Style::default().fg(Color::Cyan)));
    status_spans.push(Span::styled(model, Style::default().fg(Color::White)));
    status_spans.push(Span::styled("  ⚡ ", Style::default().fg(Color::Magenta)));
    status_spans.push(Span::styled(variant_str, Style::default().fg(Color::White)));
    status_spans.push(Span::styled("  📂 ", Style::default().fg(Color::DarkGray)));
    status_spans.push(Span::styled(ws_name, Style::default().fg(Color::DarkGray)));
    frame.render_widget(Paragraph::new(Line::from(status_spans)), chunks[1]);

    if !app.matched_commands.is_empty() {
        render_command_palette(app, frame, chunks[0]);
    }
}

/// 按选区命中将单行文本切分为 normal / selected 两种样式的 span 序列
/// （以全局字符下标判定，`char_offset` 为该行首字符的全局下标）
fn line_selection_spans(
    line_str: &str,
    char_offset: usize,
    selection: (usize, usize),
    normal: Style,
    selected: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut chunk = String::new();
    let mut in_sel: Option<bool> = None;

    for (local_idx, c) in line_str.chars().enumerate() {
        let global_idx = char_offset + local_idx;
        let hit = global_idx >= selection.0 && global_idx < selection.1;
        if in_sel == Some(hit) {
            chunk.push(c);
        } else {
            if !chunk.is_empty() {
                let style = if in_sel == Some(true) {
                    selected
                } else {
                    normal
                };
                spans.push(Span::styled(std::mem::take(&mut chunk), style));
            }
            chunk.push(c);
            in_sel = Some(hit);
        }
    }

    if !chunk.is_empty() {
        let style = if in_sel == Some(true) {
            selected
        } else {
            normal
        };
        spans.push(Span::styled(chunk, style));
    }
    spans
}

/// 渲染命令建议浮层（置于输入框上方，支持上下键高亮、Tab 快速补全）
fn render_command_palette(app: &App, frame: &mut Frame, input_rect: Rect) {
    let count = app.matched_commands.len().min(8);
    let popup_height = (count as u16) + 2;
    let popup_width = 64.min(input_rect.width);

    let popup_y = if input_rect.y >= popup_height {
        input_rect.y - popup_height
    } else {
        input_rect.y + input_rect.height
    };

    let popup_rect = Rect {
        x: input_rect.x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // 清除背景，避免被下层文字穿透
    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Commands (↑/↓ to select, Tab to complete, Enter to execute) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let mut lines = Vec::new();
    for (i, cmd) in app.matched_commands.iter().take(count).enumerate() {
        let is_selected = i == app.selected_command_index;
        let prefix = if is_selected { "> " } else { "  " };

        let style = if is_selected {
            selected_row_style(Color::Cyan)
        } else {
            Style::default().fg(Color::White)
        };

        let desc_style = if is_selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        lines.push(command_row(
            cmd.name,
            cmd.description,
            prefix,
            12,
            style,
            desc_style,
        ));
    }

    let p = Paragraph::new(lines).block(block);
    frame.render_widget(p, popup_rect);
}
