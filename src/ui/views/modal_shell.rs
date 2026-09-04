//! 模态弹窗视图的公共外壳：居中布局、边框标题、过滤行、
//! 滚动窗口计算、选中行样式与底部按键提示行，
//! 以及"顶部状态区 + 列表区 + 底部提示区"的整体渲染骨架。

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

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

/// 模态弹窗骨架的渲染参数
pub struct ModalShellSpec<'a> {
    pub title: &'a str,
    pub accent: Color,
    /// 首选窗口宽度（列）
    pub width: u16,
    /// 首选窗口高度（行）
    pub height: u16,
    /// 顶部状态/过滤区高度（行数）
    pub header_height: u16,
}

/// 模态弹窗整体骨架：居中窗口 + Clear + 边框 + 三段垂直分割
/// （顶部状态/过滤区、列表区、底部提示区）。
/// 顶部行与底部提示由调用方给出；返回列表区 Rect 供调用方渲染条目。
pub fn render_modal_shell<'a>(
    frame: &mut Frame,
    area: Rect,
    spec: ModalShellSpec<'_>,
    header_lines: Vec<Line<'a>>,
    footer: Line<'a>,
) -> Rect {
    let modal_rect = centered_rect(area, spec.width, spec.height);
    frame.render_widget(Clear, modal_rect);

    let block = modal_block(spec.title, spec.accent);
    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(spec.header_height), // 顶部状态与搜索条件
            Constraint::Min(1),                     // 列表
            Constraint::Length(1),                  // 底部操作按键提示
        ])
        .split(inner);

    frame.render_widget(Paragraph::new(header_lines), chunks[0]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
    chunks[1]
}

/// 列表空态提示：留空一行 + 黄色主文案 + 可选的深灰补充说明行
pub fn empty_state(message: &str, extra: Option<&str>) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(message.to_owned(), Style::default().fg(Color::Yellow)),
        ]),
    ];
    if let Some(extra) = extra {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(extra.to_owned(), Style::default().fg(Color::DarkGray)),
        ]));
    }
    lines
}

/// 列表行前缀与整行样式：选中 `> `（accent 底色反白）、活跃 `* `（黄色加粗）、普通 `  `（白色）
pub fn list_row_affix(is_selected: bool, is_active: bool, accent: Color) -> (&'static str, Style) {
    if is_selected {
        ("> ", selected_row_style(accent))
    } else if is_active {
        (
            "* ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("  ", Style::default().fg(Color::White))
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
        spans.push(Span::styled(
            (*desc).to_owned(),
            Style::default().fg(Color::DarkGray),
        ));
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
/// 首行保留给表头，选中项滚出视口下方时窗口跟随。
pub fn visible_window(selected: usize, viewport_height: usize) -> (usize, usize) {
    let visible = viewport_height.saturating_sub(1);
    let start = if visible > 0 && selected >= visible {
        selected - visible + 1
    } else {
        0
    };
    (start, visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visible_window_reserves_header_row() {
        // 视口 2 行 = 表头 1 行 + 仅剩 1 条可见（回归：过滤后单条目曾渲染 0 行）
        assert_eq!(visible_window(0, 2), (0, 1));
        assert_eq!(visible_window(0, 1), (0, 0));

        // 未滚动时从头展示
        assert_eq!(visible_window(0, 11), (0, 10));

        // 选中项滚出视口下方时窗口跟随
        assert_eq!(visible_window(9, 11), (0, 10));
        assert_eq!(visible_window(10, 11), (1, 10));
    }
}
