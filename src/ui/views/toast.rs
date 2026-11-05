use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};

use crate::ui::app::App;
use crate::ui::app::toast::ToastLevel;
use crate::ui::views::{bold, fg};

/// 最后绘制的右上角浮层，不参与聊天区布局。
pub fn render_toast(app: &App, frame: &mut Frame) -> Rect {
    let Some(toast) = app.feedback.current() else {
        return Rect::default();
    };
    let color = match toast.level {
        ToastLevel::Info => Color::Cyan,
        ToastLevel::Success => Color::Green,
        ToastLevel::Warning => Color::Yellow,
        ToastLevel::Error => Color::Red,
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            match toast.level {
                ToastLevel::Info => "通知",
                ToastLevel::Success => "成功",
                ToastLevel::Warning => "警告",
                ToastLevel::Error => "错误",
            },
            bold(color),
        ),
        Span::styled("  Esc 关闭", fg(Color::DarkGray)),
    ])];
    lines.extend(toast.text.lines().map(|line| Line::raw(line.to_owned())));
    if app.feedback.pending() > 0 {
        lines.push(Line::from(Span::styled(
            format!("+{} 条通知", app.feedback.pending()),
            fg(Color::DarkGray),
        )));
    }
    let size = frame.area();
    let width = (lines.iter().map(Line::width).max().unwrap() + 6)
        .min(60)
        .min(size.width.saturating_sub(6) as usize) as u16;
    let block = Block::new()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(fg(color))
        .padding(Padding::new(2, 2, 1, 1));
    let paragraph = Paragraph::new(lines)
        .style(Style::new().fg(Color::White).bg(Color::Rgb(30, 32, 38)))
        .wrap(Wrap { trim: false })
        .block(block);
    let height = paragraph
        .line_count(width.saturating_sub(6))
        .min(size.height.saturating_sub(2) as usize) as u16;
    let area = Rect::new(
        size.right().saturating_sub(width + 2),
        size.y + 1,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
    area
}
