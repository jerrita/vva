//! 共享的 span 切分工具。

use ratatui::style::Style;
use ratatui::text::Span;

/// 按字符走查将文本切成连续同样式的 span：相邻字符命中状态一致则合并。
/// 命中判定由调用方闭包给出（参数为字符在 `text` 中的序号与字符本身），
/// 坐标换算留在调用方。
pub(crate) fn split_spans_by_hit(
    text: &str,
    normal: Style,
    selected: Style,
    mut is_hit: impl FnMut(usize, char) -> bool,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut chunk = String::new();
    let mut in_hit: Option<bool> = None;
    for (idx, ch) in text.chars().enumerate() {
        let hit = is_hit(idx, ch);
        if in_hit == Some(hit) {
            chunk.push(ch);
        } else {
            if !chunk.is_empty() {
                let style = if in_hit == Some(true) {
                    selected
                } else {
                    normal
                };
                spans.push(Span::styled(std::mem::take(&mut chunk), style));
            }
            chunk.push(ch);
            in_hit = Some(hit);
        }
    }
    if !chunk.is_empty() {
        let style = if in_hit == Some(true) {
            selected
        } else {
            normal
        };
        spans.push(Span::styled(chunk, style));
    }
    spans
}
