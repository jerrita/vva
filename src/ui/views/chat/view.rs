//! 终端视口：面板、滚动与选区合成的最终绘制。

use super::selection::*;
use super::*;

pub(super) const SCROLLBAR_THUMB: &str = "│";
pub(super) const SCROLLBAR_TRACK: &str = "│";

/// 聊天流正文可用宽度（右侧留出一列空白）
fn chat_text_width(area: Rect) -> usize {
    (area.width as usize).saturating_sub(1)
}

pub fn render_chat(app: &mut App, frame: &mut Frame, area: Rect) {
    let title = app.view.session.header.title.as_deref().unwrap_or("");
    let block = panel_block()
        .title(session_title_line(app, title))
        .padding(Padding::new(1, 1, 1, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let status = app.view.state.status();
    let show_status = !status.is_empty() && inner.height > 1;
    let viewport = if inner.width > 1 && inner.height > 0 {
        let status_height = if show_status { 2 } else { 0 };
        let height = inner.height.saturating_sub(status_height);
        Rect { height, ..inner }
    } else {
        area
    };
    app.chat.area = viewport;
    app.scrollbar_area = Rect::new(
        area.right().saturating_sub(1),
        viewport.y,
        u16::from(area.width > 0),
        viewport.height,
    );

    app.refresh_chat_lines(chat_text_width(viewport));
    let total_lines = app.chat.lines.len();
    let viewport_height = viewport.height as usize;
    let max_scroll = total_lines.saturating_sub(viewport_height);

    app.update_viewport(max_scroll, viewport_height);

    let scroll = app.chat_scroll();
    let start = scroll.min(total_lines);
    let end = (scroll + viewport_height).min(total_lines);
    let selection = app.chat.selection.get().copied();
    let text_width = viewport.width.saturating_sub(1);

    {
        let window = &app.chat.lines[start..end];
        for (i, line) in window.iter().enumerate() {
            frame.render_widget(
                line,
                Rect {
                    x: viewport.x,
                    y: viewport.y + i as u16,
                    width: text_width,
                    height: 1,
                },
            );
        }
        if let Some(sel) = selection {
            overlay_selection(frame.buffer_mut(), viewport, window, &sel, scroll);
        }
    }

    // 轨道与右边框重合，保留上下圆角。
    if max_scroll > 0 {
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll);

        let thumb_color = if app.is_dragging_scrollbar {
            Color::Rgb(150, 180, 225)
        } else {
            Color::Rgb(130, 140, 158)
        };

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol(SCROLLBAR_THUMB)
            .track_symbol(Some(SCROLLBAR_TRACK))
            .thumb_style(Style::default().fg(thumb_color))
            .track_style(Style::default().fg(PANEL_BORDER));

        frame.render_stateful_widget(scrollbar, app.scrollbar_area, &mut scrollbar_state);
    }

    if show_status {
        frame.render_widget(
            Line::from(Span::styled(
                format!("{} {status}", app.spinner_char()),
                Style::default().fg(Color::Yellow),
            )),
            Rect {
                x: inner.x,
                y: inner.bottom().saturating_sub(1),
                width: inner.width,
                height: 1,
            },
        );
    }
}
