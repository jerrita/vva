//! 文本展示工具：按字符显示宽度（CJK 计双列）安全截断，多字节内容不会 panic 或溢出窄栏。

use unicode_width::UnicodeWidthChar;

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

fn prefix_by_width(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let cw = char_width(ch);
        if width + cw > max {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out
}

/// 截断到最多 `max` 个显示列，超长时以 "..." 结尾（总宽度仍不超过 `max`）。
pub fn truncate(text: &str, max: usize) -> String {
    truncate_with(text, max, "...")
}

/// 截断到最多 `max` 个显示列，超长时以自定义省略号结尾。
pub fn truncate_with(text: &str, max: usize, ellipsis: &str) -> String {
    let ellipsis_width: usize = ellipsis.chars().map(char_width).sum();
    let total_width: usize = text.chars().map(char_width).sum();
    if total_width <= max {
        return text.to_string();
    }
    // max 容不下省略号时退化为纯宽度截断前缀
    if max <= ellipsis_width {
        return prefix_by_width(text, max);
    }
    let mut out = prefix_by_width(text, max - ellipsis_width);
    out.push_str(ellipsis);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_text() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn truncate_appends_ellipsis_within_limit() {
        assert_eq!(truncate("abcdefghij", 8), "abcde...");
        assert_eq!(truncate_with("abcdefghij", 8, ".."), "abcdef..");
    }

    #[test]
    fn truncate_is_multibyte_safe_and_width_aware() {
        // CJK 每字符占 2 显示列：宽度预算 6 时只能保留 1 个字符 + "..."
        let cjk = "会话标题可以很长很长很长";
        let truncated = truncate(cjk, 6);
        let width: usize = truncated.chars().map(char_width).sum();
        assert!(truncated.ends_with("..."));
        assert!(width <= 6, "truncated width {width} exceeds budget");

        // 纯 ASCII 行为与字符数语义一致
        assert_eq!(truncate("abcdefghij", 8), "abcde...");
    }

    #[test]
    fn truncate_cjk_does_not_overflow_narrow_columns() {
        // 30 列侧栏标题：按显示宽度截断后不得超出
        let title = "会话标题可以很长很长很长很长很长";
        let truncated = truncate(title, 18);
        let width: usize = truncated.chars().map(char_width).sum();
        assert!(width <= 18);
    }

    #[test]
    fn truncate_handles_tiny_limit() {
        assert_eq!(truncate("abcdef", 2), "ab");
        assert_eq!(truncate("abcdef", 0), "");
    }
}
