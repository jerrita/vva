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
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut in_hit = false;
    for (idx, ch) in text.chars().enumerate() {
        let hit = is_hit(idx, ch);
        match spans.last_mut() {
            Some(last) if in_hit == hit => last.content.to_mut().push(ch),
            _ => spans.push(Span::styled(
                ch.to_string(),
                if hit { selected } else { normal },
            )),
        }
        in_hit = hit;
    }
    spans
}
