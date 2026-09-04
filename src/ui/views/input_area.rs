use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::app::App;

/// 复合输入区：状态指示条 + 命令浮层 + 输入框 + 底部元数据栏
pub fn render_input_area(
    app: &App,
    frame: &mut Frame,
    area: Rect,
    model: &str,
    variant: Option<&str>,
    workspace: &str,
    status_text: &str,
) {
    let inner_box_height = area.height.saturating_sub(2);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 输入框上方状态栏 (Agent 运行状态 / 网络搜索)
            Constraint::Length(inner_box_height), // 输入框
            Constraint::Length(1), // 输入框下方状态栏 (模型/推理档位/提示)
        ])
        .split(area);

    // 1. 输入框上方状态行
    let is_running = app.active_session.is_running();
    let agent_status_spans = if is_running {
        let text = if status_text.is_empty() {
            "Working..."
        } else {
            status_text
        };
        vec![
            Span::styled(
                format!(" {} ", app.spinner_char()),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
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

    let web_search_span = if app.enable_web_search {
        Span::styled("🌐 Web Search: Enabled  ", Style::default().fg(Color::Green))
    } else {
        Span::styled("🌐 Web Search: Off  ", Style::default().fg(Color::DarkGray))
    };

    let mut top_line_spans = agent_status_spans;
    top_line_spans.push(Span::raw("    "));
    top_line_spans.push(web_search_span);

    let top_bar = Paragraph::new(Line::from(top_line_spans));
    frame.render_widget(top_bar, chunks[0]);

    // 2. 输入框 (支持多行与视口滚动)
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(if is_running {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Rgb(80, 80, 85))
        });

    let inner_width = chunks[1].width.saturating_sub(2) as usize;
    let inner_height = chunks[1].height.saturating_sub(2) as usize;
    let (cursor_row, cursor_col) = app.cursor_line_and_col();

    // 视口平滑滚动计算
    let mut scroll_y = app.input_scroll_y.get();
    if cursor_row < scroll_y {
        scroll_y = cursor_row;
    } else if inner_height > 0 && cursor_row >= scroll_y + inner_height {
        scroll_y = cursor_row + 1 - inner_height;
    }
    app.input_scroll_y.set(scroll_y);

    let mut scroll_x = app.input_scroll_x.get();
    if cursor_col < scroll_x {
        scroll_x = cursor_col;
    } else if inner_width > 0 && cursor_col >= scroll_x + inner_width {
        scroll_x = cursor_col + 1 - inner_width;
    }
    app.input_scroll_x.set(scroll_x);

    let input_p = if app.input.is_empty() {
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Type a prompt or / for commands... (Enter to send, Ctrl+Enter for newline)",
                Style::default().fg(Color::Rgb(100, 100, 105)),
            ),
        ]))
        .block(input_block)
    } else {
        Paragraph::new(app.input.as_str())
            .block(input_block)
            .scroll((scroll_y as u16, scroll_x as u16))
    };

    frame.render_widget(input_p, chunks[1]);

    // 绘制光标 (基于行号和列显示宽度定位，正确支持多行、视口滚动及全角 CJK 字符)
    if !is_running
        && inner_height > 0
        && inner_width > 0
        && cursor_row >= scroll_y
        && cursor_row < scroll_y + inner_height
    {
        let row_offset = (cursor_row - scroll_y) as u16;
        let col_offset = cursor_col.saturating_sub(scroll_x) as u16;
        let cursor_x = chunks[1].x + 1 + col_offset;
        let cursor_y = chunks[1].y + 1 + row_offset;
        if cursor_x < chunks[1].x + chunks[1].width - 1
            && cursor_y < chunks[1].y + chunks[1].height - 1
        {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    // 3. 输入框下方状态行 (模型、推理强度、项目)
    let ws_name = std::path::Path::new(workspace)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(workspace);

    let variant_str = variant.unwrap_or("default");
    let bottom_line = Line::from(vec![
        Span::styled("🤖 ", Style::default().fg(Color::Cyan)),
        Span::styled(model, Style::default().fg(Color::White)),
        Span::styled("  ⚡ ", Style::default().fg(Color::Magenta)),
        Span::styled(variant_str, Style::default().fg(Color::White)),
        Span::styled("  📂 ", Style::default().fg(Color::DarkGray)),
        Span::styled(ws_name, Style::default().fg(Color::DarkGray)),
        Span::styled("  ·  ", Style::default().fg(Color::DarkGray)),
        Span::styled("/model", Style::default().fg(Color::Cyan)),
        Span::styled(" /variant", Style::default().fg(Color::Magenta)),
        Span::styled(" /config", Style::default().fg(Color::Yellow)),
        Span::styled(" /sessions", Style::default().fg(Color::Cyan)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled("/help", Style::default().fg(Color::Cyan)),
        Span::styled(" for help", Style::default().fg(Color::DarkGray)),
    ]);
    let bottom_bar = Paragraph::new(bottom_line);
    frame.render_widget(bottom_bar, chunks[2]);

    // 4. 命令浮层 (Claude Code / OpenCode 风格)
    if !app.matched_commands.is_empty() {
        render_command_palette(app, frame, chunks[1]);
    }
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
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));

    let mut lines = Vec::new();
    for (i, cmd) in app.matched_commands.iter().take(count).enumerate() {
        let is_selected = i == app.selected_command_index;
        let prefix = if is_selected { "> " } else { "  " };

        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let desc_style = if is_selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("{}{:<12}", prefix, cmd.name), style),
            Span::styled(format!(" - {}", cmd.description), desc_style),
        ]));
    }

    let p = Paragraph::new(lines).block(block);
    frame.render_widget(p, popup_rect);
}

