use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use crossterm::cursor::{Hide, RestorePosition, SavePosition, Show};
use crossterm::queue;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use super::app::TextPane;

#[derive(Default)]
pub(super) struct Hyperlinks {
    // Screen coordinates are ordered by row, then column. Each entry represents
    // one rendered glyph rather than every terminal cell occupied by that glyph.
    previous: BTreeMap<(u16, u16), LinkSnapshot>,
    previous_area: Option<Rect>,
}

#[derive(Clone, PartialEq)]
struct LinkSnapshot {
    cell: Cell,
    url: String,
}

pub(super) struct LinkUpdate {
    x: u16,
    y: u16,
    cell: Cell,
    url: Option<String>,
}

impl Hyperlinks {
    pub fn prepare(
        &mut self,
        buffer: &Buffer,
        pane: &TextPane,
        scroll: usize,
        enabled: bool,
    ) -> Vec<LinkUpdate> {
        let current = if enabled {
            linked_cells(buffer, pane, scroll)
        } else {
            BTreeMap::new()
        };

        let force_repaint = self.previous_area != Some(buffer.area);
        self.previous_area = Some(buffer.area);
        let mut touched: BTreeSet<_> = self.previous.keys().copied().collect();
        touched.extend(current.keys().copied());
        let rows: BTreeSet<_> = touched.iter().map(|&(y, _)| y).collect();
        let mut next = BTreeMap::new();
        let mut updates = Vec::new();
        for y in rows {
            let mut x = buffer.area.x;
            while let Some(cell) = buffer.cell((x, y)) {
                let end = x.saturating_add(cell.symbol().width().max(1) as u16);
                if cell.diff_option != CellDiffOption::Skip {
                    let url = (x..end).find_map(|col| current.get(&(y, col))).cloned();
                    let previous = (x..end).find_map(|col| self.previous.get(&(y, col)));
                    if let Some(url) = url {
                        let snapshot = LinkSnapshot {
                            cell: cell.clone(),
                            url: url.clone(),
                        };
                        if force_repaint || previous != Some(&snapshot) {
                            updates.push(LinkUpdate {
                                x,
                                y,
                                cell: cell.clone(),
                                url: Some(url),
                            });
                        }
                        next.insert((y, x), snapshot);
                    } else if previous.is_some() {
                        updates.push(LinkUpdate {
                            x,
                            y,
                            cell: cell.clone(),
                            url: None,
                        });
                    }
                }
                if end <= x {
                    break;
                }
                x = end;
            }
        }
        self.previous = next;
        updates
    }
}

/// 视口内可点击的 http(s) 链接所覆盖的屏幕单元格 → 目标 URL
fn linked_cells(buffer: &Buffer, pane: &TextPane, scroll: usize) -> BTreeMap<(u16, u16), String> {
    let mut cells = BTreeMap::new();
    for link in &pane.links {
        let Some(row) = link.line.checked_sub(scroll) else {
            continue;
        };
        if row >= pane.area.height as usize || link.url.chars().any(char::is_control) {
            continue;
        }
        let Ok(url) = reqwest::Url::parse(&link.url) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            continue;
        }
        let end = link
            .columns
            .end
            .min(pane.area.width.saturating_sub(1) as usize);
        for col in link.columns.start..end {
            let position = (pane.area.y + row as u16, pane.area.x + col as u16);
            if buffer.cell((position.1, position.0)).is_some() {
                cells.insert(position, url.to_string());
            }
        }
    }
    cells
}

/// Ratatui diffs text/style only. Repaint linked cells from the completed frame so
/// changed labels receive new targets, and removed links are explicitly cleared.
pub(super) fn write_updates<W: Write>(
    backend: &mut CrosstermBackend<W>,
    updates: &[LinkUpdate],
) -> io::Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    queue!(backend, SavePosition, Hide)?;
    let repaint = (|| {
        for group in updates.chunk_by(|a, b| a.url == b.url) {
            write!(
                backend,
                "\x1b]8;;{}\x1b\\",
                group[0].url.as_deref().unwrap_or("")
            )?;
            backend.draw(
                group
                    .iter()
                    .map(|update| (update.x, update.y, &update.cell)),
            )?;
            write!(backend, "\x1b]8;;\x1b\\")?;
        }
        Ok(())
    })();
    let restore = queue!(backend, RestorePosition, Show).and_then(|()| Write::flush(backend));
    repaint.and(restore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::markdown::LinkRegion;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};

    fn fixture() -> (Buffer, TextPane) {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 10));
        buffer.set_string(13, 7, "中文 link", Style::default().fg(Color::Cyan));
        let pane = TextPane {
            area: Rect::new(10, 5, 12, 4),
            links: vec![LinkRegion {
                line: 12,
                columns: 3..12,
                url: "https://example.com/first".into(),
            }],
            ..Default::default()
        };
        (buffer, pane)
    }

    #[test]
    fn osc8_output_preserves_wide_cells_style_and_hides_cursor() {
        let (buffer, pane) = fixture();
        let updates = Hyperlinks::default().prepare(&buffer, &pane, 10, true);
        assert_eq!(
            updates.iter().map(|u| u.x).collect::<Vec<_>>(),
            [13, 15, 17, 18, 19, 20]
        );
        assert!(updates.iter().all(|u| u.y == 7 && u.cell.fg == Color::Cyan));
        let mut output = Vec::new();
        let mut backend = CrosstermBackend::new(&mut output);
        write_updates(&mut backend, &updates).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("\x1b7\x1b[?25l\x1b]8;;https://example.com/first\x1b\\"));
        assert!(output.contains('中'));
        assert!(output.contains('文'));
        assert!(output.ends_with("\x1b]8;;\x1b\\\x1b8\x1b[?25h"));
    }

    #[test]
    fn unchanged_labels_update_targets_and_clear_for_modals() {
        let (buffer, mut pane) = fixture();
        let mut links = Hyperlinks::default();
        links.prepare(&buffer, &pane, 10, true);
        assert!(links.prepare(&buffer, &pane, 10, true).is_empty());
        pane.links[0].url = "https://example.com/second".into();
        let updates = links.prepare(&buffer, &pane, 10, true);
        assert!(
            updates
                .iter()
                .all(|u| u.url.as_deref() == Some("https://example.com/second"))
        );
        let updates = links.prepare(&buffer, &pane, 10, false);
        assert!(!updates.is_empty());
        assert!(updates.iter().all(|u| u.url.is_none()));
        let mut output = Vec::new();
        let mut backend = CrosstermBackend::new(&mut output);
        write_updates(&mut backend, &updates).unwrap();
        assert!(!String::from_utf8(output).unwrap().contains("https://"));
        assert!(links.prepare(&buffer, &pane, 10, false).is_empty());
        assert!(
            links
                .prepare(&buffer, &pane, 10, true)
                .iter()
                .all(|u| u.url.is_some())
        );
        pane.reset();
        assert!(
            links
                .prepare(&buffer, &pane, 10, true)
                .iter()
                .all(|u| u.url.is_none())
        );
    }

    #[test]
    fn scrolling_and_resizing_clear_old_positions() {
        let (buffer, pane) = fixture();
        let mut links = Hyperlinks::default();
        links.prepare(&buffer, &pane, 10, true);
        let updates = links.prepare(&buffer, &pane, 11, true);
        assert!(updates.iter().any(|u| u.y == 6 && u.url.is_some()));
        assert!(updates.iter().filter(|u| u.y == 7).all(|u| u.url.is_none()));
        let small = Buffer::empty(Rect::new(0, 0, 15, 7));
        let updates = links.prepare(&small, &pane, 11, true);
        assert!(updates.iter().all(|u| u.x < 15 && u.y < 7));
        let updates = links.prepare(&small, &pane, 20, true);
        assert!(updates.iter().all(|u| u.url.is_none()));
    }

    #[test]
    fn removed_link_on_wide_continuation_repaints_whole_glyph() {
        let (mut buffer, mut pane) = fixture();
        pane.links[0].columns = 4..5;
        buffer.set_string(13, 7, "ab", Style::default());
        let mut links = Hyperlinks::default();
        assert_eq!(links.prepare(&buffer, &pane, 10, true)[0].x, 14);
        buffer.set_string(13, 7, "中", Style::default());
        pane.links.clear();
        let updates = links.prepare(&buffer, &pane, 10, true);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].x, 13);
        assert_eq!(updates[0].cell.symbol(), "中");
        assert!(updates[0].url.is_none());
    }

    #[test]
    fn unsafe_targets_do_not_emit_terminal_links() {
        let (buffer, mut pane) = fixture();
        for url in [
            "javascript:alert(1)",
            "file:///tmp/test",
            "https://example.com/\x1b]52;c;bad\x07",
            "https://example.com/\n",
        ] {
            pane.links[0].url = url.into();
            assert!(
                Hyperlinks::default()
                    .prepare(&buffer, &pane, 10, true)
                    .is_empty()
            );
        }
    }
}
