use std::cell::Cell;
use std::ops::Deref;

/// 多行文本输入缓冲区，封装文本操作、多字节 Unicode 边界安全、光标移动及视口滚动状态
#[derive(Debug, Clone, Default)]
pub struct InputBuffer {
    pub text: String,
    pub cursor_pos: usize, // 字符索引偏移（0..chars().count()）
    pub scroll_y: Cell<usize>,
    pub scroll_x: Cell<usize>,
}

impl Deref for InputBuffer {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

impl InputBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 重置输入内容并归零光标与视口滚动
    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor_pos = self.text.chars().count();
        self.scroll_y.set(0);
        self.scroll_x.set(0);
    }

    /// 清空输入框内容并归零光标与视口滚动
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
        self.scroll_y.set(0);
        self.scroll_x.set(0);
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 计算输入框文本行数
    pub fn line_count(&self) -> usize {
        if self.text.is_empty() {
            1
        } else {
            self.text.split('\n').count()
        }
    }

    /// 计算光标当前所在的行号 (0-indexed) 以及在该行中的显示列宽度 (0-indexed，支持全角 CJK 字符)
    pub fn cursor_line_and_col(&self) -> (usize, usize) {
        let byte_offset = self
            .text
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_pos)
            .unwrap_or(self.text.len());
        let prefix = &self.text[..byte_offset];
        let lines: Vec<&str> = prefix.split('\n').collect();
        let cursor_line = lines.len().saturating_sub(1);
        let current_line_prefix = lines.last().copied().unwrap_or("");
        let cursor_col = ratatui::text::Span::raw(current_line_prefix).width();
        (cursor_line, cursor_col)
    }

    /// 插入单字符 (支持多字节 Unicode/中文字符)
    pub fn insert_char(&mut self, c: char) {
        let byte_offset = self
            .text
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_pos)
            .unwrap_or(self.text.len());
        self.text.insert(byte_offset, c);
        self.cursor_pos += 1;
    }

    /// 粘贴或批量插入字符串 (支持多行与多字节 Unicode，自动归一化 CRLF/CR 为 \n)
    pub fn insert_str(&mut self, s: &str) {
        let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
        let byte_offset = self
            .text
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_pos)
            .unwrap_or(self.text.len());
        self.text.insert_str(byte_offset, &normalized);
        self.cursor_pos += normalized.chars().count();
    }

    /// 退格删除字符 (Backspace)
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 && !self.text.is_empty() {
            self.cursor_pos -= 1;
            if let Some((byte_offset, _)) = self.text.char_indices().nth(self.cursor_pos) {
                self.text.remove(byte_offset);
            }
        }
    }

    /// 删除光标后字符 (Delete)
    pub fn delete(&mut self) {
        let char_count = self.text.chars().count();
        if self.cursor_pos < char_count
            && let Some((byte_offset, _)) = self.text.char_indices().nth(self.cursor_pos)
        {
            self.text.remove(byte_offset);
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.text.chars().count() {
            self.cursor_pos += 1;
        }
    }

    /// 光标向上移动一行，在上一行中尽量对齐当前列的视觉显示宽度
    pub fn move_cursor_up(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let all_lines: Vec<&str> = self.text.split('\n').collect();
        if all_lines.len() <= 1 {
            return;
        }

        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1; // +1 对应 '\n'
        }

        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };

        if cursor_row > 0 {
            let target_row = cursor_row - 1;
            let target_line = all_lines[target_row];
            let (_, current_visual_col) = self.cursor_line_and_col();
            let target_char_col = char_idx_for_visual_width(target_line, current_visual_col);
            self.cursor_pos = line_starts[target_row] + target_char_col;
        }
    }

    /// 光标向下移动一行，在下一行中尽量对齐当前列的视觉显示宽度
    pub fn move_cursor_down(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let all_lines: Vec<&str> = self.text.split('\n').collect();
        if all_lines.len() <= 1 {
            return;
        }

        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1; // +1 对应 '\n'
        }

        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };

        if cursor_row + 1 < all_lines.len() {
            let target_row = cursor_row + 1;
            let target_line = all_lines[target_row];
            let (_, current_visual_col) = self.cursor_line_and_col();
            let target_char_col = char_idx_for_visual_width(target_line, current_visual_col);
            self.cursor_pos = line_starts[target_row] + target_char_col;
        }
    }

    /// 光标移动到当前行行首；若已在当前行行首，则移动到整个文本最开头
    pub fn move_cursor_home(&mut self) {
        if self.text.is_empty() || self.cursor_pos == 0 {
            self.cursor_pos = 0;
            return;
        }
        let all_lines: Vec<&str> = self.text.split('\n').collect();
        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1;
        }
        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        let line_start = line_starts[cursor_row];
        if self.cursor_pos == line_start {
            self.cursor_pos = 0;
        } else {
            self.cursor_pos = line_start;
        }
    }

    /// 光标移动到当前行行尾；若已在当前行行尾，则移动到整个文本最末尾
    pub fn move_cursor_end(&mut self) {
        let total_chars = self.text.chars().count();
        if self.text.is_empty() || self.cursor_pos >= total_chars {
            self.cursor_pos = total_chars;
            return;
        }
        let all_lines: Vec<&str> = self.text.split('\n').collect();
        let mut line_starts: Vec<usize> = Vec::with_capacity(all_lines.len());
        let mut cur_char = 0;
        for l in &all_lines {
            line_starts.push(cur_char);
            cur_char += l.chars().count() + 1;
        }
        let cursor_row = match line_starts.binary_search(&self.cursor_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        let line_end = line_starts[cursor_row] + all_lines[cursor_row].chars().count();
        if self.cursor_pos == line_end {
            self.cursor_pos = total_chars;
        } else {
            self.cursor_pos = line_end;
        }
    }
}

/// 根据目标视觉宽度在给定单行文本中寻找最匹配的字符索引
pub fn char_idx_for_visual_width(line: &str, target_width: usize) -> usize {
    let mut current_width: usize = 0;
    let mut best_idx = 0;
    let mut best_diff = target_width;

    for (char_idx, c) in line.chars().enumerate() {
        let diff = current_width.abs_diff(target_width);
        if diff < best_diff {
            best_diff = diff;
            best_idx = char_idx;
        }
        let w = ratatui::text::Span::raw(c.to_string()).width();
        current_width += w;
        let diff_after = current_width.abs_diff(target_width);
        if diff_after < best_diff {
            best_diff = diff_after;
            best_idx = char_idx + 1;
        }
        if current_width >= target_width {
            break;
        }
    }
    best_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unicode_chinese_input_and_editing() {
        let mut buf = InputBuffer::new();

        // 1. 连续输入中文字符，严禁触发 byte index not a char boundary panic
        buf.insert_char('你');
        buf.insert_char('好');
        buf.insert_char('世');
        buf.insert_char('界');
        assert_eq!(buf.text, "你好世界");
        assert_eq!(buf.cursor_pos, 4);

        // 2. 光标向左移动到 "你好" 与 "世界" 之间
        buf.move_cursor_left();
        buf.move_cursor_left();
        assert_eq!(buf.cursor_pos, 2);

        // 3. 在中间插入中文字符
        buf.insert_char('，');
        assert_eq!(buf.text, "你好，世界");
        assert_eq!(buf.cursor_pos, 3);

        // 4. Backspace 删除逗号
        buf.backspace();
        assert_eq!(buf.text, "你好世界");
        assert_eq!(buf.cursor_pos, 2);

        // 5. Delete 删除后面的 "世"
        buf.delete();
        assert_eq!(buf.text, "你好界");
        assert_eq!(buf.cursor_pos, 2);

        // 6. 光标 Home 与 End
        buf.move_cursor_home();
        assert_eq!(buf.cursor_pos, 0);
        buf.move_cursor_end();
        assert_eq!(buf.cursor_pos, 3);

        // 7. 批量粘贴中文字符串
        buf.insert_str("！欢迎使用");
        assert_eq!(buf.text, "你好界！欢迎使用");
        assert_eq!(buf.cursor_pos, 8);
    }

    #[test]
    fn test_multiline_paste_and_line_counting() {
        let mut buf = InputBuffer::new();

        // 1. 初始单行
        assert_eq!(buf.line_count(), 1);
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 0);
        assert_eq!(col, 0);

        // 2. 粘贴包含 CRLF 与 CR 的多行内容，验证统一归一化为 \n
        buf.insert_str("def foo():\r\n    msg = '你好'\r    return msg\n");
        assert_eq!(buf.text, "def foo():\n    msg = '你好'\n    return msg\n");
        assert_eq!(buf.line_count(), 4);

        // 3. 验证光标在第 3 行开头 (第四行，空行)
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 3);
        assert_eq!(col, 0);

        // 4. 测试 Up 向上移动
        buf.move_cursor_up();
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 2);
        assert_eq!(col, 0);

        // 5. 移到行尾并测试 visual width 对齐
        buf.move_cursor_end();
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 2);
        assert_eq!(col, 14); // "    return msg" 长度 14

        // 向上移动到第 1 行："    msg = '你好'" (视觉宽度 4 + 7 + 4 = 15)
        buf.move_cursor_up();
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 1);
        assert!(col >= 13);

        // 向上移动到第 0 行："def foo():" (视觉宽度 10)
        buf.move_cursor_up();
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 0);
        assert_eq!(col, 10); // 对齐至行尾

        // 测试 Home 行首与文档首
        buf.move_cursor_home();
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 0);
        assert_eq!(col, 0);

        // 向下移动
        buf.move_cursor_down();
        let (row, col) = buf.cursor_line_and_col();
        assert_eq!(row, 1);
        assert_eq!(col, 0);
    }

    #[test]
    fn test_set_and_clear() {
        let mut buf = InputBuffer::new();
        buf.set("hello\nworld");
        assert_eq!(buf.line_count(), 2);
        assert_eq!(buf.cursor_pos, 11);

        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.cursor_pos, 0);
        assert_eq!(buf.scroll_y.get(), 0);
        assert_eq!(buf.scroll_x.get(), 0);
    }
}
