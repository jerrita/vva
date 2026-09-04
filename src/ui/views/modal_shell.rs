//! 模态弹窗视图的公共外壳：居中布局、边框标题、过滤行、
//! 滚动窗口计算、选中行样式与底部按键提示行。

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};

/// 在给定区域内按首选尺寸居中（宽高均不越过区域的 4 行/列边距）
pub fn centered_rect(area: Rect, prefer_width: u16, prefer_height: u16) -> Rect {
    let width = prefer_width.min(area.width.saturating_sub(4));
    let height = prefer_height.min(area.height.saturating_sub(4));
    Rect {
        x: (area.width - width) / 2,
        y: (area.height - height) / 2,
        width,
        height,
    }
}

/// 模态弹窗的边框与标题（accent 同时作用于边框与标题文字）
pub fn modal_block(title: &str, accent: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(
            title.to_owned(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
}

/// 过滤词输入行：`  Filter: <query>_`
pub fn filter_line(query: &str, accent: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled("  Filter: ", Style::default().fg(Color::DarkGray)),
        Span::styled(query.to_owned(), Style::default().fg(accent)),
        Span::styled("_", Style::default().fg(accent)),
    ])
}

/// 底部按键提示行：`[key] desc` 片段序列（key 样式由调用方给定，desc 固定 DarkGray）
pub fn footer_hints(hints: &[(&str, Style, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, key_style, desc) in hints {
        spans.push(Span::styled((*key).to_owned(), *key_style));
        spans.push(Span::styled((*desc).to_owned(), Style::default().fg(Color::DarkGray)));
    }
    Line::from(spans)
}

/// 列表选中行样式（黑字 + accent 底色 + 加粗）
pub fn selected_row_style(accent: Color) -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(accent)
        .add_modifier(Modifier::BOLD)
}

/// 计算选中项可见的滚动窗口：(起始下标, 可见条数)。
/// 最后一行保留给表头，选中项滚出视口下方时窗口跟随。
pub fn visible_window(selected: usize, viewport_height: usize) -> (usize, usize) {
    let visible = viewport_height.saturating_sub(1);
    let start = if selected >= visible {
        selected - visible + 1
    } else {
        0
    };
    (start, visible)
}
