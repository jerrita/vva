use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::ui::text::truncate;

#[derive(Debug, Clone)]
pub struct LinkRegion {
    pub line: usize,
    pub columns: std::ops::Range<usize>,
    pub url: String,
}

#[derive(Default)]
struct MarkdownLinks {
    active: Option<String>,
    pending: std::collections::HashMap<usize, String>,
    regions: Vec<LinkRegion>,
}

impl MarkdownLinks {
    fn track(&mut self, span_index: usize) {
        if let Some(url) = &self.active {
            self.pending.insert(span_index, url.clone());
        }
    }

    fn record(&mut self, span: usize, line: usize, column: usize, width: usize) {
        if let Some(url) = self.pending.get(&span) {
            if let Some(last) = self.regions.last_mut()
                && last.line == line
                && last.columns.end == column
                && last.url == *url
            {
                last.columns.end += width;
            } else {
                self.regions.push(LinkRegion {
                    line,
                    columns: column..column + width,
                    url: url.clone(),
                });
            }
        }
    }
}

/// 将包含 CJK 与普通字符的 Span 切割并按终端可用宽度安全折行
/// initial_indent: 首行前缀（如 "   " 或 "   • "）
/// cont_indent: 折行后续行前缀（如 "   " 或 "     "）
pub fn wrap_spans_to_lines(
    spans: &[Span<'static>],
    max_width: usize,
    initial_indent: &str,
    cont_indent: &str,
) -> Vec<Line<'static>> {
    wrap_spans(
        spans,
        max_width,
        initial_indent,
        cont_indent,
        |_, _, _, _| {},
    )
}

fn wrap_spans(
    spans: &[Span<'static>],
    max_width: usize,
    initial_indent: &str,
    cont_indent: &str,
    mut record: impl FnMut(usize, usize, usize, usize),
) -> Vec<Line<'static>> {
    let max_width = max_width.max(20);
    let mut result: Vec<Line<'static>> = Vec::new();

    let init_indent_w = initial_indent.width();
    let cont_indent_w = cont_indent.width();

    let mut current_spans: Vec<Span<'static>> = vec![Span::raw(initial_indent.to_string())];
    let mut current_width = init_indent_w;

    for (span_index, span) in spans.iter().enumerate() {
        let style = span.style;
        let text = &span.content;

        // 将文本切分为 Token（ASCII 空格、ASCII 单词、单个 CJK 字符）
        let tokens = tokenize_text(text);

        for token in tokens {
            let tok_w = token.width();

            // 如果当前行放不下此 token，且当前行已有内容（不仅是缩进）
            if current_width + tok_w > max_width && current_width > cont_indent_w {
                // 如果是行尾空白，直接跳过换行
                if token.trim().is_empty() {
                    continue;
                }

                // 提交当前行
                result.push(Line::from(std::mem::take(&mut current_spans)));

                // 开启新行并注入续行缩进
                current_spans.push(Span::raw(cont_indent.to_string()));
                current_width = cont_indent_w;

                // 新行开头的多余空格丢弃
                if token.trim().is_empty() {
                    continue;
                }
            }

            // 追加到当前行
            record(span_index, result.len(), current_width, tok_w);
            if let Some(last) = current_spans.last_mut().filter(|last| last.style == style) {
                last.content.to_mut().push_str(token);
            } else {
                current_spans.push(Span::styled(token.to_owned(), style));
            }
            current_width += tok_w;
        }
    }

    if current_spans.len() > 1 || (current_spans.len() == 1 && current_width > init_indent_w) {
        result.push(Line::from(current_spans));
    }

    if result.is_empty() {
        result.push(Line::from(vec![Span::raw(initial_indent.to_string())]));
    }

    result
}

/// 将字符串切分为微 Token：
/// - 连续的 ASCII 空格
/// - 连续的 ASCII 非空白字符（英文单词、数字、ASCII 标点）
/// - 每一个非 ASCII 字符（如中文汉字、全角标点、Emoji）作为一个独立折行断点
fn tokenize_text(text: &str) -> impl Iterator<Item = &str> {
    let mut remaining = text;
    std::iter::from_fn(move || {
        let mut chars = remaining.char_indices();
        let (_, first) = chars.next()?;
        let end = if first.is_ascii() {
            chars
                .find(|(_, ch)| {
                    !ch.is_ascii() || ch.is_ascii_whitespace() != first.is_ascii_whitespace()
                })
                .map_or(remaining.len(), |(idx, _)| idx)
        } else {
            first.len_utf8()
        };
        let (token, rest) = remaining.split_at(end);
        remaining = rest;
        Some(token)
    })
}

/// 检查字符是否属于标点符号（含 ASCII 标点与常见全角 CJK 标点）
fn is_cjk_or_ascii_punct(ch: char) -> bool {
    ch.is_ascii_punctuation()
        || matches!(
            ch,
            '：' | '！'
                | '？'
                | '；'
                | '，'
                | '。'
                | '、'
                | '）'
                | '】'
                | '”'
                | '’'
                | '》'
                | '…'
                | '—'
                | '·'
        )
}

/// 对 Markdown 文本进行 CJK 标点强调边界预处理
///
/// 处理 CommonMark 规范在 CJK 语境下的边界条件：
/// 当加粗块以标点结尾且紧随汉字时（例如 `**标题：**内容` 或 `**提示！**请注意`），
/// CommonMark 规范要求右侧闭合标记满足 right-flanking 条件（紧随标点且后继为非空白非标点字符的分隔符不满足该条件），
/// 导致 `pulldown-cmark` 无法闭合 Strong 标签并输出原始 `**` 标记。
/// 本函数在闭合标记后注入空格，使其满足 right-flanking 分隔符条件。
pub fn preprocess_cjk_markdown(input: &str) -> String {
    let mut result = String::with_capacity(input.len() + 64);
    let mut in_code_block = false;

    for line in input.lines() {
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
        } else if !in_code_block {
            result.push_str(&normalize_cjk_bold_in_line(line));
            result.push('\n');
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }

    if !input.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

fn normalize_cjk_bold_in_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    let mut open_stack: Vec<usize> = Vec::new();
    let mut in_inline_code = false;

    while i < n {
        // 行内代码 `...` 原样跳过
        if chars[i] == '`' {
            in_inline_code = !in_inline_code;
            out.push('`');
            i += 1;
            continue;
        }

        if in_inline_code {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // 检查连续星号
        if chars[i] == '*' {
            let start = i;
            while i < n && chars[i] == '*' {
                i += 1;
            }
            let count = i - start;

            // 优先检查是否可以闭合栈顶已有的强调
            if let Some(&top_count) = open_stack.last()
                && top_count == count
            {
                open_stack.pop();
                for _ in 0..count {
                    out.push('*');
                }

                // 核心逻辑：若前驱字符是标点，且后继字符是 CJK 汉字或字母数字（非空白非标点），
                // CommonMark 会判定其无法闭合，因此在星号后注入空格使其合法闭合
                let prev_is_punct = start > 0 && is_cjk_or_ascii_punct(chars[start - 1]);
                let next_is_word = if i < n {
                    let next_ch = chars[i];
                    !next_ch.is_whitespace() && !is_cjk_or_ascii_punct(next_ch)
                } else {
                    false
                };

                if prev_is_punct && next_is_word {
                    out.push(' ');
                }
                continue;
            }

            // 检查是否可以作为开启标记：后继字符非空白
            let next_is_ws = i < n && chars[i].is_whitespace();
            if !next_is_ws && (count == 2 || count == 3 || count == 1) {
                open_stack.push(count);
            }

            for _ in 0..count {
                out.push('*');
            }
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// Markdown 原生渲染器：将 CommonMark 文本解析并排版为具备美化样式的 Ratatui Line 列表
pub fn render_markdown(input: &str, max_width: usize) -> (Vec<Line<'static>>, Vec<LinkRegion>) {
    let (mut output_lines, links) = render_markdown_open(input, max_width, false);
    // 去除末尾冗余空行
    while let Some(last) = output_lines.last() {
        if strippable_line(last) {
            output_lines.pop();
        } else {
            break;
        }
    }
    (output_lines, links)
}

pub(crate) fn strippable_line(line: &Line<'_>) -> bool {
    line.spans.is_empty() || (line.spans.len() == 1 && line.spans[0].content.trim().is_empty())
}

/// 单次 Markdown 渲染的块级状态与输出缓冲
struct MarkdownRenderer {
    max_width: usize,
    /// 本次调用之前已有输出（影响标题与代码块前的空行判定）
    preceded: bool,
    output_lines: Vec<Line<'static>>,
    links: MarkdownLinks,
    bold: bool,
    italic: bool,
    strikethrough: bool,
    current_heading_level: Option<HeadingLevel>,
    list_stack: Vec<Option<u64>>,
    in_blockquote: bool,
    in_code_block: bool,
    code_block_lang: String,
    code_block_lines: Vec<String>,
    in_table: bool,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    /// 普通行 Span 收集器
    pending_spans: Vec<Span<'static>>,
}

impl MarkdownRenderer {
    fn new(max_width: usize, preceded: bool) -> Self {
        Self {
            max_width,
            preceded,
            output_lines: Vec::new(),
            links: MarkdownLinks::default(),
            bold: false,
            italic: false,
            strikethrough: false,
            current_heading_level: None,
            list_stack: Vec::new(),
            in_blockquote: false,
            in_code_block: false,
            code_block_lang: String::new(),
            code_block_lines: Vec::new(),
            in_table: false,
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            pending_spans: Vec::new(),
        }
    }

    /// 将挂起的普通行 Span 按列表/引用缩进排版并写入输出
    fn flush_paragraph(&mut self) {
        if self.pending_spans.is_empty() {
            return;
        }

        let (init_indent, cont_indent) = match self.list_stack.last() {
            Some(Some(num)) => (format!("   {num}. "), "      ".to_string()),
            Some(None) => ("   • ".to_string(), "     ".to_string()),
            None if self.in_blockquote => ("   │ ".to_string(), "   │ ".to_string()),
            None => ("   ".to_string(), "   ".to_string()),
        };

        let offset = self.output_lines.len();
        let spans = std::mem::take(&mut self.pending_spans);
        let wrapped = wrap_spans(
            &spans,
            self.max_width,
            &init_indent,
            &cont_indent,
            |span, line, col, width| {
                self.links.record(span, offset + line, col, width);
            },
        );
        self.output_lines.extend(wrapped);
        self.links.pending.clear();
    }

    /// 标题行：图标 + 挂起 Span，并为链接记录命中区域
    fn push_heading(&mut self) {
        let level = self
            .current_heading_level
            .take()
            .unwrap_or(HeadingLevel::H1);
        let (icon, style) = heading_decoration(level);

        let mut heading_spans = vec![Span::raw("   "), Span::styled(icon, style)];
        let mut column = 3 + icon.width();
        for (index, span) in self.pending_spans.iter().enumerate() {
            self.links
                .record(index, self.output_lines.len(), column, span.width());
            column += span.width();
        }
        self.links.pending.clear();
        heading_spans.append(&mut self.pending_spans);
        self.output_lines.push(Line::from(heading_spans));
        self.output_lines.push(Line::from(""));
    }

    fn end_code_block(&mut self) {
        self.in_code_block = false;
        if self.preceded || !self.output_lines.is_empty() {
            self.output_lines.push(Line::from(""));
        }
        self.output_lines
            .push(code_box_header_line(&self.code_block_lang, self.max_width));
        for code_l in &self.code_block_lines {
            self.output_lines
                .push(code_box_line(code_l, self.max_width));
        }
        self.output_lines.push(code_box_footer_line(self.max_width));
        self.output_lines.push(Line::from(""));
    }

    fn handle_start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush_paragraph();
                if self.preceded || !self.output_lines.is_empty() {
                    self.output_lines.push(Line::from(""));
                }
                self.current_heading_level = Some(level);
            }
            Tag::Paragraph => {
                // 段落开始
            }
            Tag::List(start_num) => {
                self.flush_paragraph();
                self.list_stack.push(start_num);
            }
            Tag::Item => self.flush_paragraph(),
            Tag::BlockQuote(_) => {
                self.flush_paragraph();
                self.in_blockquote = true;
            }
            Tag::CodeBlock(kind) => {
                self.flush_paragraph();
                self.in_code_block = true;
                self.code_block_lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_block_lines.clear();
            }
            Tag::Table(_) => {
                self.flush_paragraph();
                self.in_table = true;
                self.table_rows.clear();
            }
            Tag::TableHead | Tag::TableRow => self.current_row.clear(),
            Tag::TableCell => self.current_cell.clear(),
            Tag::Emphasis => self.italic = true,
            Tag::Strong => self.bold = true,
            Tag::Strikethrough => self.strikethrough = true,
            Tag::Link { dest_url, .. } => self.links.active = Some(dest_url.into_string()),
            _ => {}
        }
    }

    fn handle_end(&mut self, tag_end: TagEnd) {
        match tag_end {
            TagEnd::Heading(_) => self.push_heading(),
            TagEnd::Paragraph => {
                self.flush_paragraph();
                self.output_lines.push(Line::from(""));
            }
            TagEnd::List(_) => {
                self.flush_paragraph();
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.output_lines.push(Line::from(""));
                }
            }
            TagEnd::Item => {
                self.flush_paragraph();
                if let Some(Some(n)) = self.list_stack.last_mut() {
                    *n += 1;
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush_paragraph();
                self.in_blockquote = false;
                self.output_lines.push(Line::from(""));
            }
            TagEnd::CodeBlock => self.end_code_block(),
            TagEnd::TableCell => self
                .current_row
                .push(std::mem::take(&mut self.current_cell)),
            TagEnd::TableHead | TagEnd::TableRow => {
                if !self.current_row.is_empty() {
                    self.table_rows.push(std::mem::take(&mut self.current_row));
                }
            }
            TagEnd::Table => {
                self.in_table = false;
                render_table(&self.table_rows, self.max_width, &mut self.output_lines);
                self.output_lines.push(Line::from(""));
            }
            TagEnd::Emphasis => self.italic = false,
            TagEnd::Strong => self.bold = false,
            TagEnd::Strikethrough => self.strikethrough = false,
            TagEnd::Link => self.links.active = None,
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.in_code_block {
            for line in text.split('\n') {
                self.code_block_lines.push(line.to_string());
            }
        } else if self.in_table {
            self.current_cell.push_str(text);
        } else {
            let mut style = Style::default().fg(Color::White);
            if self.bold {
                style = style
                    .fg(Color::Rgb(255, 215, 80))
                    .add_modifier(Modifier::BOLD);
            }
            if self.italic {
                style = style
                    .add_modifier(Modifier::ITALIC)
                    .fg(Color::Rgb(200, 200, 210));
            }
            if self.strikethrough {
                style = style.add_modifier(Modifier::CROSSED_OUT);
            }
            if self.links.active.is_some() {
                style = style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
            }
            if self.in_blockquote {
                style = style
                    .fg(Color::Rgb(170, 175, 185))
                    .add_modifier(Modifier::ITALIC);
            }

            self.links.track(self.pending_spans.len());
            self.pending_spans
                .push(Span::styled(text.to_string(), style));
        }
    }

    fn push_code(&mut self, code: &str) {
        if self.in_table {
            self.current_cell.push_str(code);
            return;
        }
        let code_style = Style::default()
            .fg(Color::Rgb(240, 195, 110))
            .bg(Color::Rgb(40, 44, 52));
        self.links.track(self.pending_spans.len());
        self.pending_spans
            .push(Span::styled(format!("`{code}`"), code_style));
    }

    fn push_soft_break(&mut self) {
        self.links.track(self.pending_spans.len());
        self.pending_spans.push(Span::raw(" "));
    }

    fn push_rule(&mut self) {
        self.flush_paragraph();
        let w = self.max_width.saturating_sub(6).max(10);
        self.output_lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("─".repeat(w), Style::default().fg(Color::Rgb(80, 80, 85))),
        ]));
        self.output_lines.push(Line::from(""));
    }

    fn finish(mut self) -> (Vec<Line<'static>>, Vec<LinkRegion>) {
        // 刷新末尾悬挂段落
        self.flush_paragraph();
        (self.output_lines, self.links.regions)
    }
}

/// 标题层级 → 图标与样式
fn heading_decoration(level: HeadingLevel) -> (&'static str, Style) {
    match level {
        HeadingLevel::H1 => (
            "█ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        HeadingLevel::H2 => (
            "■ ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        HeadingLevel::H3 => (
            "◆ ",
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        _ => (
            "▸ ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

/// 增量渲染用开放尾渲染：不剥除末尾空行；`preceded` 表示本次调用之前已有输出，
/// 影响标题与代码块前的空行判定。
pub(crate) fn render_markdown_open(
    input: &str,
    max_width: usize,
    preceded: bool,
) -> (Vec<Line<'static>>, Vec<LinkRegion>) {
    let preprocessed = preprocess_cjk_markdown(input);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut renderer = MarkdownRenderer::new(max_width, preceded);
    for event in Parser::new_ext(&preprocessed, options) {
        match event {
            Event::Start(tag) => renderer.handle_start(tag),
            Event::End(tag_end) => renderer.handle_end(tag_end),
            Event::Text(text) => renderer.push_text(&text),
            Event::Code(code) => renderer.push_code(&code),
            Event::SoftBreak | Event::HardBreak => renderer.push_soft_break(),
            Event::Rule => renderer.push_rule(),
            _ => {}
        }
    }
    renderer.finish()
}

/// 代码盒/表格边框共用灰
const BOX_BORDER_STYLE: Style = Style::new().fg(Color::Rgb(100, 100, 110));
/// 代码盒头部行固定装饰宽度：`   ┌── [tag] …┐` 中语言标签与补线之外的列数
const CODE_BOX_HEADER_CHROME: usize = 11;
/// 代码盒内容行两侧装饰宽度：`   │ … │`
const CODE_BOX_BODY_CHROME: usize = 7;
/// 代码盒尾部行装饰宽度：`   └…┘`
const CODE_BOX_FOOTER_CHROME: usize = 5;

/// 代码盒头部行（语言标签来自围栏 info string 或为空）
pub(crate) fn code_box_header_line(lang: &str, max_width: usize) -> Line<'static> {
    let lang_tag = truncate(
        if lang.is_empty() { "code" } else { lang },
        max_width.saturating_sub(CODE_BOX_HEADER_CHROME + 1),
    );
    let header_line = format!(
        "   ┌── [{lang_tag}] {}┐",
        "─".repeat(max_width.saturating_sub(CODE_BOX_HEADER_CHROME + lang_tag.width()))
    );
    Line::from(vec![Span::styled(header_line, BOX_BORDER_STYLE)])
}

/// 代码盒内容行（超宽截断，右侧补齐）
pub(crate) fn code_box_line(content: &str, max_width: usize) -> Line<'static> {
    let code_width = max_width.saturating_sub(CODE_BOX_BODY_CHROME).max(13);
    let code_l = truncate(content, code_width);
    let padding = " ".repeat(code_width.saturating_sub(code_l.width()));
    Line::from(vec![
        Span::styled("   │ ", BOX_BORDER_STYLE),
        Span::styled(code_l, Style::default().fg(Color::Rgb(220, 225, 230))),
        Span::styled(format!("{padding} │"), BOX_BORDER_STYLE),
    ])
}

pub(crate) fn code_box_footer_line(max_width: usize) -> Line<'static> {
    let footer_line = format!(
        "   └{}┘",
        "─".repeat(max_width.saturating_sub(CODE_BOX_FOOTER_CHROME))
    );
    Line::from(vec![Span::styled(footer_line, BOX_BORDER_STYLE)])
}

/// 表格边框行：`prefix` 起始（含缩进）、列间 `join`、末尾 `end`
fn table_border(prefix: &str, join: char, end: char, col_widths: &[usize]) -> String {
    let mut border = String::from(prefix);
    for (i, &w) in col_widths.iter().enumerate() {
        border.push_str(&"─".repeat(w + 2));
        border.push(if i + 1 < col_widths.len() { join } else { end });
    }
    border
}

/// 格式化渲染美观的 Unicode 表格
fn render_table(rows: &[Vec<String>], max_width: usize, output: &mut Vec<Line<'static>>) {
    if rows.is_empty() {
        return;
    }

    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }

    // 计算各列最大宽度，并将整表限制在可用区域内。
    let mut col_widths = vec![1usize; col_count];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            col_widths[i] = col_widths[i].max(cell.width()).min(40);
        }
    }
    let border_and_padding = 3 * col_count + 4;
    let content_width = max_width.saturating_sub(border_and_padding).max(col_count);
    while col_widths.iter().sum::<usize>() > content_width {
        let Some((widest, _)) = col_widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 1)
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        col_widths[widest] -= 1;
    }

    let border_style = BOX_BORDER_STYLE;
    let header_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let cell_style = Style::default().fg(Color::White);

    // 顶边框 ┌──────┬──────┐
    output.push(Line::from(vec![Span::styled(
        table_border("   ┌", '┬', '┐', &col_widths),
        border_style,
    )]));

    // 数据行与分隔线
    for (row_idx, row) in rows.iter().enumerate() {
        let wrapped_cells: Vec<Vec<String>> = col_widths
            .iter()
            .enumerate()
            .map(|(col_idx, &width)| {
                wrap_table_cell(row.get(col_idx).map(String::as_str).unwrap_or(""), width)
            })
            .collect();
        let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1);
        let row_style = if row_idx == 0 {
            header_style
        } else {
            cell_style
        };

        for line_idx in 0..row_height {
            let mut line_spans = vec![Span::raw("   "), Span::styled("│", border_style)];
            for (col_idx, &width) in col_widths.iter().enumerate() {
                let content = wrapped_cells[col_idx]
                    .get(line_idx)
                    .map(String::as_str)
                    .unwrap_or("");
                let pad = width.saturating_sub(content.width());
                let padded_cell = format!(" {content}{} ", " ".repeat(pad));
                line_spans.push(Span::styled(padded_cell, row_style));
                line_spans.push(Span::styled("│", border_style));
            }
            output.push(Line::from(line_spans));
        }

        // 表头下边框 ├──────┼──────┤
        if row_idx == 0 && rows.len() > 1 {
            output.push(Line::from(vec![Span::styled(
                table_border("   ├", '┼', '┤', &col_widths),
                border_style,
            )]));
        }
    }

    // 底边框 └──────┴──────┘
    output.push(Line::from(vec![Span::styled(
        table_border("   └", '┴', '┘', &col_widths),
        border_style,
    )]));
}

fn wrap_table_cell(cell: &str, max_width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut line_width = 0;

    for ch in cell.chars() {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch == '\n' || (line_width > 0 && line_width + width > max_width) {
            while lines.last().is_some_and(|line| line.ends_with(' ')) {
                lines.last_mut().unwrap().pop();
            }
            lines.push(String::new());
            line_width = 0;
            if ch == '\n' {
                continue;
            }
        }
        if line_width == 0 && ch.is_whitespace() {
            continue;
        }
        lines.last_mut().unwrap().push(ch);
        line_width += width;
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_follow_wrapping_and_keep_distinct_destinations() {
        let (lines, links) = render_markdown(
            "- [中文 **bold** long link label](https://example.com/first) [same](https://example.com/a)[same](https://example.com/b)",
            24,
        );
        let text_for = |url: &str| {
            links
                .iter()
                .filter(|link| link.url == url)
                .map(|link| {
                    let mut column = 0;
                    lines[link.line]
                        .spans
                        .iter()
                        .flat_map(|span| span.content.chars())
                        .filter(|ch| {
                            let hit = link.columns.contains(&column);
                            column += unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(0);
                            hit
                        })
                        .collect::<String>()
                })
                .collect::<String>()
        };
        assert_eq!(
            text_for("https://example.com/first"),
            "中文 bold long linklabel"
        );
        assert_eq!(text_for("https://example.com/a"), "same");
        assert_eq!(text_for("https://example.com/b"), "same");
        assert!(
            links
                .iter()
                .filter(|link| link.url.ends_with("/first"))
                .count()
                > 1
        );
        for link in &links {
            assert!(link.columns.start >= 5);
            assert!(link.columns.end <= 24);
        }
    }

    #[test]
    fn heading_and_inline_code_links_preserve_targets() {
        let (_, links) = render_markdown(
            "# [Title](https://example.com/title)\n\n[`code`](https://example.com/code) plain",
            80,
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].line, 0);
        assert_eq!(links[0].columns, 5..10);
        assert_eq!(links[1].columns, 3..9);
        assert_eq!(links[1].url, "https://example.com/code");
    }

    #[test]
    fn test_markdown_headings_clean_presentation() {
        let md = "# Title 1\n## Title 2\n### 国内新闻";
        let (lines, _) = render_markdown(md, 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        // 校验不包含未解析的 ###
        assert!(!text.contains("###"));
        assert!(text.contains("国内新闻"));
        assert!(text.contains("Title 1"));
        assert!(text.contains("Title 2"));
    }

    #[test]
    fn test_markdown_chinese_list_no_orphan_bullet() {
        let md = "- 示例条目：这是一段用于验证中文长文本折行行为的泛化内容，其中包含多个短语和标点符号。([example.com](https://example.com/article))\n- 后续条目：用于验证列表的第二项内容。";
        let (lines, _) = render_markdown(md, 60);

        // 验证 bullet 与中文在同一行开头连接，不产生孤立 bullet 独占一行
        for line in &lines {
            let row_str: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            // 校验不存在仅包含 "•" 的孤立行
            assert_ne!(row_str.trim(), "•");
        }

        // 验证链接不重复展开为 `(url)` 形式
        let full_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(full_text.contains("example.com"));
        assert!(!full_text.contains("(https://example.com"));
    }

    #[test]
    fn test_markdown_cjk_bold_with_punctuation_rendered_properly() {
        let md = "* **分类一：**这里是第一段示例内容。\n* **分类二：**这里是第二段示例内容。\n* **提示！**这里是提示内容。";
        let (lines, _) = render_markdown(md, 80);

        // 验证没有未闭合的字面量 **
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            !all_text.contains("**"),
            "Markdown should not contain literal asterisks for bold titles: {}",
            all_text
        );
        assert!(all_text.contains("分类一："));
        assert!(all_text.contains("第一段示例内容"));
        assert!(all_text.contains("分类二："));
        assert!(all_text.contains("提示！"));

        // 验证加粗 Span 具有金色与 BOLD 属性
        let bold_span_found = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.content.contains("分类一：")
                    && s.style.add_modifier.contains(Modifier::BOLD)
                    && s.style.fg == Some(Color::Rgb(255, 215, 80))
            })
        });
        assert!(
            bold_span_found,
            "Bold span with golden color and BOLD modifier should exist"
        );
    }

    #[test]
    fn test_markdown_code_block_and_table() {
        let md = r#"```rust
fn test() {
    println!("hi");
}
```

| Name | Role |
| --- | --- |
| Alice | Admin |
| Bob | User |
"#;
        let (lines, _) = render_markdown(md, 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("[rust]"));
        assert!(text.contains("println!"));
        assert!(text.contains("Alice"));
        assert!(text.contains("Admin"));
        assert!(text.contains("Bob"));

        let code_start = lines
            .iter()
            .position(|line| line.to_string().contains("┌── [rust]"))
            .unwrap();
        let code_end = lines
            .iter()
            .position(|line| line.to_string().starts_with("   └"))
            .unwrap();
        assert!(lines[code_start].to_string().ends_with('┐'));
        assert!(lines[code_end].to_string().ends_with('┘'));
        for line in &lines[code_start..=code_end] {
            assert_eq!(line.width(), 80);
        }
    }

    #[test]
    fn table_wraps_long_cells_within_available_width() {
        let md = "| 项目 | 状态 | 详细说明 |\n\
                  | --- | --- | --- |\n\
                  | 示例甲 | 已启用 | 这是一段需要在有限列宽内自动折行的较长中文说明文本 |\n\
                  | 示例乙 | 待处理 | 第二段说明同时包含 English words 和中文字符 |";
        let max_width = 72;
        let (lines, _) = render_markdown(md, max_width);

        assert!(lines.iter().all(|line| line.width() <= max_width));
        assert!(
            lines.len() > 7,
            "long table cells should create wrapped rows"
        );

        let text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(text.contains("自动折行"));
        assert!(text.contains("English words"));
    }

    #[test]
    fn table_keeps_inline_code_in_cells() {
        let md = "| Tool | Path | Return |\n\
                  | --- | --- | --- |\n\
                  | `edit` | `path: string` | `{ files, replacements }` |";
        let (lines, _) = render_markdown(md, 80);
        let rows: Vec<String> = lines.iter().map(Line::to_string).collect();

        assert!(rows.iter().any(|row| {
            row.contains("edit") && row.contains("path: string") && row.contains("files")
        }));
        assert!(!rows.iter().any(|row| row.contains('`')));
    }

    #[test]
    fn test_wrap_spans_cjk_hanging_indent() {
        let spans = vec![Span::raw(
            "这是一个非常长的中文段落测试，用于检验自适应折行逻辑是否能够正确在汉字边界处截断换行并为每一行保持正确的缩进。",
        )];
        let lines = wrap_spans_to_lines(&spans, 30, "   • ", "     ");
        assert!(lines.len() > 1);

        // 首行必须包含 "   • "
        let first_row: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_row.starts_with("   • "));

        // 续行必须以 "     " 开始
        for line in &lines[1..] {
            let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(row.starts_with("     "));
        }
    }

    #[test]
    fn test_markdown_inline_formatting() {
        let md = "This is **bold**, *italic*, ~~deleted~~, and `inline code`. > Quote block";
        let (lines, _) = render_markdown(md, 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("bold"));
        assert!(text.contains("italic"));
        assert!(text.contains("deleted"));
        assert!(text.contains("`inline code`"));
    }

    #[test]
    fn test_tokenize_text_boundary_cases() {
        let cases: &[(&str, &[&str])] = &[
            ("", &[]),
            ("   \t\r\n", &["   \t\r\n"]),
            ("hello world", &["hello", " ", "world"]),
            ("hello  \t  world", &["hello", "  \t  ", "world"]),
            ("a中b文c", &["a", "中", "b", "文", "c"]),
            ("🦀Rust中文", &["🦀", "Rust", "中", "文"]),
            ("e\u{301}x", &["e", "\u{301}", "x"]),
            ("foo\t\r\nbar", &["foo", "\t\r\n", "bar"]),
        ];

        for &(input, expected) in cases {
            let actual: Vec<&str> = tokenize_text(input).collect();
            assert_eq!(actual, expected, "failed on input: {:?}", input);
        }
    }

    #[test]
    fn test_wrap_spans_style_and_indent_preservation() {
        let s1 = Style::default().fg(Color::Red);
        let s2 = Style::default().fg(Color::Green);

        let spans = vec![
            Span::styled("hello", s1),
            Span::styled(" world", s1),
            Span::styled(" test", s2),
        ];
        let lines = wrap_spans_to_lines(&spans, 80, "> ", "  ");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(lines[0].spans[0].content, "> ");
        assert_eq!(lines[0].spans[1].content, "hello world");
        assert_eq!(lines[0].spans[1].style, s1);
        assert_eq!(lines[0].spans[2].content, " test");
        assert_eq!(lines[0].spans[2].style, s2);

        let spans = [
            Span::styled("一二三四五六七八", s1),
            Span::styled("九十一二三四五六", s2),
        ];
        assert_eq!(
            wrap_spans_to_lines(&spans, 20, "• ", "  "),
            vec![
                Line::from(vec![
                    Span::raw("• "),
                    Span::styled("一二三四五六七八", s1),
                    Span::styled("九", s2)
                ]),
                Line::from(vec![Span::raw("  "), Span::styled("十一二三四五六", s2)]),
            ]
        );
    }
}
