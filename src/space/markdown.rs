//! IM 命令输出的 Markdown 排版辅助。

/// 动态文本压平空白并转义 Markdown 标记，保持表格与行内内容完整。
pub(super) fn text(value: &str) -> String {
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for ch in flat.chars() {
        if matches!(
            ch,
            '\\' | '|' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '~'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 带标题的 Markdown 表格；调用方传入未经转义的单元格。
pub(super) fn table<const N: usize>(
    title: &str,
    headers: [&str; N],
    rows: impl IntoIterator<Item = [String; N]>,
) -> String {
    let mut out = format!(
        "### {title}\n\n| {} |\n| {} |",
        headers.join(" | "),
        ["---"; N].join(" | ")
    );
    for row in rows {
        let cells = row.map(|value| text(&value));
        out.push_str(&format!("\n| {} |", cells.join(" | ")));
    }
    out
}
