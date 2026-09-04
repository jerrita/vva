//! 文本展示工具：按字符（非字节）安全截断，多字节内容不会 panic。

/// 截断到最多 `max` 个字符，超长时以 "..." 结尾（总宽度仍不超过 `max`）。
pub fn truncate(text: &str, max: usize) -> String {
    truncate_with(text, max, "...")
}

/// 截断到最多 `max` 个字符，超长时以自定义省略号结尾。
pub fn truncate_with(text: &str, max: usize, ellipsis: &str) -> String {
    let ellipsis_len = ellipsis.chars().count();
    if max <= ellipsis_len {
        return text.chars().take(max).collect();
    }
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max - ellipsis_len).collect();
    truncated.push_str(ellipsis);
    truncated
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
    fn truncate_is_multibyte_safe() {
        // 旧实现使用字节切片，中文内容会在这里 panic
        let cjk = "会话标题可以很长很长很长";
        let truncated = truncate(cjk, 6);
        assert_eq!(truncated.chars().count(), 6);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn truncate_handles_tiny_limit() {
        assert_eq!(truncate("abcdef", 2), "ab");
        assert_eq!(truncate("abcdef", 0), "");
    }
}
