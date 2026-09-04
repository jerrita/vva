use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::ui::app::App;
use crate::ui::text::truncate;
use crate::ui::views::chat::apply_selection_highlight;

/// 右侧边栏所需的会话元信息
pub struct SidebarInfo<'a> {
    pub session_id: &'a str,
    pub session_title: &'a str,
    pub workspace: &'a str,
    pub model: &'a str,
    pub variant: Option<&'a str>,
}

/// 构建侧边栏可见文本行，供渲染与复制共用。
pub fn build_sidebar_lines(app: &App, info: &SidebarInfo) -> Vec<Line<'static>> {
    let title_display = if info.session_title.trim().is_empty() {
        "Untitled".to_string()
    } else {
        truncate(info.session_title, 18)
    };

    let short_id = truncate(info.session_id, 16);

    let ws_display = std::path::Path::new(info.workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(info.workspace);

    let model_display = if info.model.is_empty() {
        "Default".to_string()
    } else {
        truncate(info.model, 20)
    };

    let context_limit = app.stats.context_limit.unwrap_or(200_000);
    let ratio = (app.stats.total_tokens as f64 / context_limit as f64).clamp(0.0, 1.0);
    let percent = (ratio * 100.0) as u16;
    let cost_str = if app.stats.estimated_cost_usd > 0.0 {
        format!("${:.4}", app.stats.estimated_cost_usd)
    } else {
        "$0.0000".to_string()
    };

    let mut lines = vec![Line::from(""); 22];
    lines[0] = Line::from(vec![
        Span::styled("Title: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            title_display,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    lines[1] = Line::from(vec![
        Span::styled("Session: ", Style::default().fg(Color::DarkGray)),
        Span::styled(short_id, Style::default().fg(Color::Cyan)),
    ]);
    lines[2] = Line::from(vec![
        Span::styled("Workspace: ", Style::default().fg(Color::DarkGray)),
        Span::styled(ws_display.to_string(), Style::default().fg(Color::White)),
    ]);
    lines[3] = Line::from(vec![
        Span::styled("Turns: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.stats.total_turns.to_string(),
            Style::default().fg(Color::Yellow),
        ),
    ]);
    lines[6] = Line::from(vec![
        Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
        Span::styled(model_display, Style::default().fg(Color::Green)),
    ]);
    lines[7] = Line::from(vec![
        Span::styled("Variant: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            info.variant.unwrap_or("none").to_string(),
            Style::default().fg(Color::Magenta),
        ),
    ]);
    lines[10] = Line::from(vec![
        Span::styled("Tokens: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{}/{}k ({}%)",
                app.stats.total_tokens,
                context_limit / 1000,
                percent
            ),
            Style::default().fg(Color::White),
        ),
    ]);
    lines[11] = Line::from(vec![
        Span::styled("In/Out: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{} / {}",
                app.stats.prompt_tokens, app.stats.completion_tokens
            ),
            Style::default().fg(Color::Rgb(170, 170, 170)),
        ),
    ]);
    lines[15] = Line::from(vec![
        Span::styled("Cache Read: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.stats.cache_read_tokens.to_string(),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    lines[16] = Line::from(vec![
        Span::styled("Cache Write: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.stats.cache_write_tokens.to_string(),
            Style::default().fg(Color::LightBlue),
        ),
    ]);
    lines[19] = Line::from(vec![
        Span::styled("Est. Cost: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            cost_str,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    lines
}

/// 右侧边栏：展示 Session ID、Workspace、缓存、上下文窗口、Token 消耗与 Cost 估算
pub fn render_sidebar(app: &mut App, frame: &mut Frame, area: Rect, info: &SidebarInfo) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 65)))
        .title(Span::styled(
            " Details ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.sidebar_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.min(22),
    };

    if inner.width < 15 || inner.height < 10 {
        app.sidebar_area = Rect::default();
        app.sidebar_lines.clear();
        app.clear_sidebar_selection();
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(5),
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(inner);

    let context_limit = app.stats.context_limit.unwrap_or(200_000);
    let ratio = (app.stats.total_tokens as f64 / context_limit as f64).clamp(0.0, 1.0);
    let percent = (ratio * 100.0) as u16;
    let mut lines = build_sidebar_lines(app, info);
    if let Some(selection) = &app.sidebar_selection {
        apply_selection_highlight(&mut lines, selection);
    }
    app.sidebar_lines = lines.clone();

    let separator = || {
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 45)))
    };

    frame.render_widget(
        Paragraph::new(lines[0..5].to_vec()).block(separator()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(lines[6..9].to_vec()).block(separator()),
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
    frame.render_widget(Paragraph::new(lines[10..12].to_vec()), token_chunks[0]);

    let gauge = Gauge::default()
        .ratio(ratio)
        .gauge_style(Style::default().fg(if percent > 80 {
            Color::Red
        } else if percent > 50 {
            Color::Yellow
        } else {
            Color::Blue
        }))
        .use_unicode(true);
    frame.render_widget(gauge, token_chunks[1]);

    frame.render_widget(
        Paragraph::new(lines[15..18].to_vec()).block(separator()),
        chunks[3],
    );
    frame.render_widget(Paragraph::new(lines[19..22].to_vec()), chunks[4]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::{TextCoord, TextSelection};
    use crate::ui::test_support::create_test_app;
    use ratatui::backend::TestBackend;

    #[tokio::test]
    async fn render_sidebar_keeps_selectable_text_lines() {
        let mut ta = create_test_app("test-model", None, None, false).await;
        let app = &mut ta.app;
        app.start_sidebar_selection(0, 7);
        app.update_sidebar_selection(0, 14);
        app.finish_sidebar_selection();

        let info = SidebarInfo {
            session_id: "session-id",
            session_title: "Example",
            workspace: "C:/workspace",
            model: "test-model",
            variant: None,
        };
        let backend = TestBackend::new(30, 25);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_sidebar(app, frame, Rect::new(0, 0, 30, 25), &info))
            .unwrap();

        assert_eq!(app.sidebar_area, Rect::new(1, 1, 29, 22));
        assert_eq!(
            app.sidebar_selection,
            Some(TextSelection::new(
                TextCoord { line: 0, col: 7 },
                TextCoord { line: 0, col: 14 }
            ))
        );
        assert_eq!(
            crate::ui::views::chat::extract_selected_text(
                &app.sidebar_lines,
                app.sidebar_selection.as_ref().unwrap(),
            ),
            Some("Example".to_string())
        );

        let _ = std::fs::remove_dir_all(&ta.temp_dir);
    }
}
