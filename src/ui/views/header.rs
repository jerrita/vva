use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::app::App;

/// 渲染顶部标题栏，展示当前 Session 标题
pub fn render_header(app: &App, frame: &mut Frame, area: Rect, title: &str) {
    let title_display = if title.trim().is_empty() {
        "Untitled Session"
    } else {
        title
    };

    let is_running = app.active_session.is_running();
    let status_color = if is_running {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    let line = Line::from(vec![
        Span::styled(
            "  💬 ",
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            title_display,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if is_running { " (Active)" } else { "" },
            Style::default().fg(status_color),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::NONE);

    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}

