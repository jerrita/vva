use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::app::App;

/// 主窗口标题：会话名与运行态
pub fn session_title_line(app: &App, title: &str) -> Line<'static> {
    let title_display = if title.trim().is_empty() {
        "Untitled Session".to_string()
    } else {
        title.to_string()
    };

    let is_running = app.active_session.is_running();
    let status_color = if is_running {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title_display,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if is_running { " (Active)" } else { "" }.to_string(),
            Style::default().fg(status_color),
        ),
        Span::raw("  "),
    ])
}
