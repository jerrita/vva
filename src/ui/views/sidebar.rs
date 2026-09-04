use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::ui::app::App;

/// 右侧边栏所需的会话元信息
pub struct SidebarInfo<'a> {
    pub session_id: &'a str,
    pub session_title: &'a str,
    pub workspace: &'a str,
    pub model: &'a str,
    pub variant: Option<&'a str>,
}

/// 右侧边栏：展示 Session ID、Workspace、缓存、上下文窗口、Token 消耗与 Cost 估算
pub fn render_sidebar(
    app: &App,
    frame: &mut Frame,
    area: Rect,
    info: &SidebarInfo,
) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 65)))
        .title(Span::styled(
            " Details ",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 15 || inner.height < 10 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // Title, Session & Workspace
            Constraint::Length(4), // Model & Variant
            Constraint::Length(5), // Context Window & Tokens
            Constraint::Length(4), // Cache Read / Write
            Constraint::Length(3), // Cost
            Constraint::Min(0),
        ])
        .split(inner);

    // 1. Session & Workspace
    let title_display = if info.session_title.trim().is_empty() {
        "Untitled".to_string()
    } else if info.session_title.chars().count() > 18 {
        format!("{}...", info.session_title.chars().take(16).collect::<String>())
    } else {
        info.session_title.to_string()
    };

    let short_id = if info.session_id.len() > 16 {
        format!("{}...", &info.session_id[..14])
    } else {
        info.session_id.to_string()
    };

    let ws_display = std::path::Path::new(info.workspace)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(info.workspace);

    let session_lines = vec![
        Line::from(vec![
            Span::styled("Title: ", Style::default().fg(Color::DarkGray)),
            Span::styled(title_display, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Session: ", Style::default().fg(Color::DarkGray)),
            Span::styled(short_id, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Workspace: ", Style::default().fg(Color::DarkGray)),
            Span::styled(ws_display, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Turns: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.stats.total_turns),
                Style::default().fg(Color::Yellow),
            ),
        ]),
    ];
    let session_p = Paragraph::new(session_lines).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 45))),
    );
    frame.render_widget(session_p, chunks[0]);

    // 2. Model & Variant
    let model_display = if info.model.len() > 20 {
        format!("{}...", &info.model[..18])
    } else if info.model.is_empty() {
        "Default".to_string()
    } else {
        info.model.to_string()
    };

    let variant_display = info.variant.unwrap_or("none");
    let model_lines = vec![
        Line::from(vec![
            Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
            Span::styled(model_display, Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::styled("Variant: ", Style::default().fg(Color::DarkGray)),
            Span::styled(variant_display, Style::default().fg(Color::Magenta)),
        ]),
    ];
    let model_p = Paragraph::new(model_lines).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 45))),
    );
    frame.render_widget(model_p, chunks[1]);

    // 3. Context Window & Tokens
    let context_limit = app.stats.context_limit.unwrap_or(200_000);
    let ratio = (app.stats.total_tokens as f64 / context_limit as f64).clamp(0.0, 1.0);
    let percent = (ratio * 100.0) as u16;

    let token_lines = vec![
        Line::from(vec![
            Span::styled("Tokens: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}/{}k ({}%)", app.stats.total_tokens, context_limit / 1000, percent),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("In/Out: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} / {}", app.stats.prompt_tokens, app.stats.completion_tokens),
                Style::default().fg(Color::Rgb(170, 170, 170)),
            ),
        ]),
    ];
    let token_p = Paragraph::new(token_lines);
    let token_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(1), Constraint::Min(0)])
        .split(chunks[2]);

    frame.render_widget(token_p, token_chunks[0]);

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

    // 4. Cache
    let cache_lines = vec![
        Line::from(vec![
            Span::styled("Cache Read: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.stats.cache_read_tokens),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(vec![
            Span::styled("Cache Write: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.stats.cache_write_tokens),
                Style::default().fg(Color::LightBlue),
            ),
        ]),
    ];
    let cache_p = Paragraph::new(cache_lines).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(40, 40, 45))),
    );
    frame.render_widget(cache_p, chunks[3]);

    // 5. Cost
    let cost_str = if app.stats.estimated_cost_usd > 0.0 {
        format!("${:.4}", app.stats.estimated_cost_usd)
    } else {
        "$0.0000".to_string()
    };

    let cost_lines = vec![Line::from(vec![
        Span::styled("Est. Cost: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            cost_str,
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
    ])];
    let cost_p = Paragraph::new(cost_lines);
    frame.render_widget(cost_p, chunks[4]);
}

