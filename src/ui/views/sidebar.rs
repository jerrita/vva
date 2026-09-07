use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::ui::app::App;
use crate::ui::text::truncate;
use crate::ui::views::chat::apply_selection_highlight;

/// 右侧边栏所需的会话元信息
pub struct SidebarInfo<'a> {
    pub session_id: &'a str,
    pub workspace: &'a str,
    pub model: &'a str,
    pub variant: Option<&'a str>,
}

/// 标签值行：左侧留白后显示 `Label: value`（标签固定 DarkGray，值样式由调用方给定）
fn label_value(label: &str, value: String, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(value, value_style),
    ])
}

fn format_number(value: impl ToString) -> String {
    let digits = value.to_string();
    let first_group_len = digits.len() % 3;
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && index % 3 == first_group_len {
            formatted.push(',');
        }
        formatted.push(ch);
    }

    formatted
}

fn format_context_limit(value: u64) -> String {
    if value >= 1_000_000 && value.is_multiple_of(1_000_000) {
        format!("{}M", format_number(value / 1_000_000))
    } else {
        format!("{}k", format_number(value / 1_000))
    }
}

/// 上下文窗口占用：(context_limit, 占用比例 0.0-1.0, 百分比)
fn context_usage(stats: &crate::ui::history::SidebarStats) -> (u64, f64, u16) {
    let context_limit = stats.context_limit.unwrap_or(200_000);
    let ratio = (stats.context_tokens as f64 / context_limit as f64).clamp(0.0, 1.0);
    (context_limit, ratio, (ratio * 100.0) as u16)
}

fn context_gauge_line(width: u16, ratio: f64, percent: u16) -> Line<'static> {
    let bar_width = width.saturating_sub(7) as usize;
    let filled = ((bar_width as f64 * ratio).round() as usize).min(bar_width);
    Line::from(vec![
        Span::raw(" ["),
        Span::styled("#".repeat(filled), Style::default().fg(Color::Blue)),
        Span::styled(
            "-".repeat(bar_width - filled),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("] {percent}%"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// 构建侧边栏可见文本行，供渲染与复制共用。
pub fn build_sidebar_lines(app: &App, info: &SidebarInfo) -> Vec<Line<'static>> {
    let short_id = truncate(info.session_id, 16);

    let ws_display = std::path::Path::new(info.workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(info.workspace);

    // model 为 "provider/model" 规格，拆分展示；缺 provider 或为空时回退占位
    let (provider_display, model_display) = match info.model.split_once('/') {
        Some((provider, model)) => (truncate(provider, 18), truncate(model, 20)),
        None => (
            "Default".to_string(),
            if info.model.is_empty() {
                "Default".to_string()
            } else {
                truncate(info.model, 20)
            },
        ),
    };

    let (context_limit, _ratio, _percent) = context_usage(&app.stats);
    let cost_str = if app.stats.estimated_cost_usd > 0.0 {
        format!("${:.4}", app.stats.estimated_cost_usd)
    } else {
        "$0.0000".to_string()
    };

    let mut lines = vec![Line::from(""); 20];
    lines[0] = label_value("Session", short_id, Style::default().fg(Color::Cyan));
    lines[1] = label_value(
        "Workspace",
        ws_display.to_string(),
        Style::default().fg(Color::White),
    );
    lines[2] = label_value(
        "Turns",
        format_number(app.stats.total_turns),
        Style::default().fg(Color::Yellow),
    );
    lines[4] = label_value("Model", model_display, Style::default().fg(Color::Green));
    lines[5] = label_value(
        "Provider",
        provider_display,
        Style::default().fg(Color::Cyan),
    );
    lines[6] = label_value(
        "Variant",
        info.variant.unwrap_or("none").to_string(),
        Style::default().fg(Color::Magenta),
    );
    lines[8] = label_value(
        "Context",
        format!(
            "{} / {}",
            format_number(app.stats.context_tokens),
            format_context_limit(context_limit)
        ),
        Style::default().fg(Color::White),
    );
    lines[9] = label_value(
        "In/Out",
        format!(
            "{} / {}",
            format_number(app.stats.prompt_tokens),
            format_number(app.stats.completion_tokens)
        ),
        Style::default().fg(Color::Rgb(170, 170, 170)),
    );
    lines[13] = label_value(
        "Cache Read",
        format_number(app.stats.cache_read_tokens),
        Style::default().fg(Color::Cyan),
    );
    lines[14] = label_value(
        "Cache Write",
        format_number(app.stats.cache_write_tokens),
        Style::default().fg(Color::LightBlue),
    );
    lines[17] = label_value(
        "Est. Cost",
        cost_str,
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    );
    lines
}

/// 右侧边栏：展示 Session ID、Workspace、缓存、上下文窗口、Token 消耗与 Cost 估算
pub fn render_sidebar(app: &mut App, frame: &mut Frame, area: Rect, info: &SidebarInfo) {
    let block = super::panel_block().title(Span::styled(
        " Details ",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.sidebar.area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.min(22),
    };

    if inner.width < 15 || inner.height < 10 {
        app.sidebar.reset();
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Length(5),
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(inner);

    let (_context_limit, ratio, percent) = context_usage(&app.stats);
    let mut lines = build_sidebar_lines(app, info);
    if let Some(selection) = app.sidebar.selection.get() {
        apply_selection_highlight(&mut lines, selection);
    }
    app.sidebar.lines = lines.clone();

    let separator = || {
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 45)))
    };

    frame.render_widget(
        Paragraph::new(lines[0..3].to_vec()).block(separator()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(lines[4..7].to_vec()).block(separator()),
        chunks[1],
    );

    let token_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(chunks[2]);
    frame.render_widget(Paragraph::new(lines[8..10].to_vec()), token_chunks[0]);
    frame.render_widget(
        Paragraph::new(context_gauge_line(token_chunks[1].width, ratio, percent)),
        token_chunks[1],
    );

    frame.render_widget(
        Paragraph::new(lines[13..16].to_vec()).block(separator()),
        chunks[3],
    );
    frame.render_widget(Paragraph::new(lines[17..20].to_vec()), chunks[4]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::{TextCoord, TextSelection};
    use crate::ui::test_support::create_test_app;
    use ratatui::backend::TestBackend;

    #[test]
    fn formats_sidebar_numbers() {
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(45_993), "45,993");
        assert_eq!(format_number(1_000_000), "1,000,000");
        assert_eq!(format_context_limit(200_000), "200k");
        assert_eq!(format_context_limit(1_000_000), "1M");
    }

    #[tokio::test]
    async fn render_sidebar_keeps_selectable_text_lines() {
        let mut ta = create_test_app("test-model", None, None, false).await;
        let app = &mut ta.app;
        app.sidebar.selection.start(0, 10);
        app.sidebar.selection.update(0, 20);
        app.sidebar.selection.finish();

        let info = SidebarInfo {
            session_id: "session-id",
            workspace: "C:/workspace",
            model: "test-model",
            variant: None,
        };
        let backend = TestBackend::new(30, 25);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_sidebar(app, frame, Rect::new(0, 0, 30, 25), &info))
            .unwrap();

        assert_eq!(app.sidebar.area, Rect::new(1, 1, 28, 22));
        assert_eq!(
            app.sidebar.selection.selection,
            Some(TextSelection::new(
                TextCoord { line: 0, col: 10 },
                TextCoord { line: 0, col: 20 }
            ))
        );
        assert_eq!(
            crate::ui::views::chat::extract_selected_text(
                &app.sidebar.lines,
                app.sidebar.selection.get().unwrap(),
            ),
            Some("session-id".to_string())
        );

        let _ = std::fs::remove_dir_all(&ta.temp_dir);
    }
}
