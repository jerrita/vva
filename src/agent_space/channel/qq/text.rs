//! QQ 文本发送辅助：分段与 Markdown 降级。

use std::sync::LazyLock;

use regex::Regex;

/// 单条消息字符上限。
pub const MAX_MESSAGE_CHARS: usize = 2000;
/// 被动回复（带 msg_id）单次最多条数。
pub const PASSIVE_REPLY_LIMIT: usize = 5;
pub const TRUNCATION_NOTICE: &str = "\n\n[回复过长，已截断。]";

/// 按字符数分段，尽量在窗口后半段的换行处断开。
///
/// 超过 `max_chunks` 时丢弃多余段，并在最后一段末尾明确写出截断提示。
pub fn chunk_text(text: &str, max_len: usize, max_chunks: Option<usize>) -> Vec<String> {
    assert!(max_len > 0, "max_len 必须为正");
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.trim().is_empty() {
        let window_end = rest
            .char_indices()
            .nth(max_len)
            .map_or(rest.len(), |(i, _)| i);
        let mut end = window_end;
        if window_end < rest.len() {
            let half = rest[..window_end]
                .char_indices()
                .nth(max_len / 2)
                .map_or(0, |(i, _)| i);
            if let Some(nl) = rest[half..window_end].rfind('\n') {
                end = half + nl + 1;
            }
        }
        let (piece, tail) = rest.split_at(end);
        if !piece.trim().is_empty() {
            chunks.push(piece.to_string());
        }
        rest = tail;
    }
    if let Some(max) = max_chunks
        && chunks.len() > max
    {
        chunks.truncate(max);
        if let Some(last) = chunks.last_mut() {
            let keep = max_len.saturating_sub(TRUNCATION_NOTICE.chars().count());
            *last = last.chars().take(keep).collect::<String>() + TRUNCATION_NOTICE;
        }
    }
    chunks
}

static MARKDOWN_RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"```\w*\n?", ""),
        (r"`([^`]+)`", "$1"),
        (r"\*\*(.+?)\*\*", "$1"),
        (r"\*(.+?)\*", "$1"),
        (r"~~(.+?)~~", "$1"),
        (r"\[([^\]]+)\]\([^)]+\)", "$1"),
        (r"(?m)^#{1,6}\s+", ""),
        (r"(?m)^\s*[-*+]\s+", "- "),
        (r"(?m)^---+$", ""),
    ]
    .into_iter()
    .map(|(pat, rep)| (Regex::new(pat).expect("静态正则合法"), rep))
    .collect()
});

/// 去除 Markdown 标记，供纯文本降级发送。
pub fn strip_markdown(text: &str) -> String {
    let mut out = text.to_string();
    for (re, rep) in MARKDOWN_RULES.iter() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_text_yields_no_chunks() {
        assert!(chunk_text("  \n ", 10, None).is_empty());
    }

    #[test]
    fn splits_by_chars_not_bytes() {
        let text = "你".repeat(25);
        let chunks = chunk_text(&text, 10, None);
        assert_eq!(
            chunks.iter().map(|c| c.chars().count()).collect::<Vec<_>>(),
            [10, 10, 5]
        );
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn prefers_newline_in_second_half() {
        let text = format!("{}\n{}", "a".repeat(15), "b".repeat(10));
        let chunks = chunk_text(&text, 20, None);
        assert_eq!(chunks, [format!("{}\n", "a".repeat(15)), "b".repeat(10)]);
    }

    #[test]
    fn truncation_is_announced_in_last_chunk() {
        let text = "x".repeat(100);
        let chunks = chunk_text(&text, 30, Some(2));
        assert_eq!(chunks.len(), 2);
        assert!(chunks[1].ends_with(TRUNCATION_NOTICE));
        assert!(chunks[1].chars().count() <= 30);
        assert!(!chunks[0].contains("截断"));
    }

    #[test]
    fn no_notice_when_within_limit() {
        let chunks = chunk_text(&"x".repeat(60), 30, Some(2));
        assert_eq!(chunks.len(), 2);
        assert!(!chunks[1].contains("截断"));
    }

    #[test]
    fn markdown_is_stripped() {
        let md = "# 标题\n**粗** `code` [链接](http://x)\n- a\n```rust\nlet x;\n```";
        assert_eq!(strip_markdown(md), "标题\n粗 code 链接\n- a\nlet x;");
    }
}
