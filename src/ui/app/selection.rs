use super::App;

/// 对话流中的文本绝对行列坐标
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCoord {
    pub line: usize,
    pub col: usize,
}

/// 鼠标拖拽文本选区
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: TextCoord,
    pub head: TextCoord,
}

impl TextSelection {
    pub fn new(anchor: TextCoord, head: TextCoord) -> Self {
        Self { anchor, head }
    }

    /// 返回规范化后的起止坐标 (start, end)，满足 start <= end
    pub fn normalized(&self) -> (TextCoord, TextCoord) {
        if self.anchor.line < self.head.line
            || (self.anchor.line == self.head.line && self.anchor.col <= self.head.col)
        {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// 判断选区是否为空（起止点重合）
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

impl App {
    pub fn start_text_selection(&mut self, line: usize, col: usize) {
        let coord = TextCoord { line, col };
        self.text_selection = Some(TextSelection::new(coord, coord));
        self.is_selecting_text = true;
        self.auto_scroll = false;
    }

    pub fn update_text_selection(&mut self, line: usize, col: usize) {
        if let Some(ref mut sel) = self.text_selection {
            sel.head = TextCoord { line, col };
        }
    }

    pub fn finish_text_selection(&mut self) {
        self.is_selecting_text = false;
        if let Some(ref sel) = self.text_selection
            && sel.is_empty()
        {
            self.text_selection = None;
        }
    }

    pub fn clear_text_selection(&mut self) {
        self.text_selection = None;
        self.is_selecting_text = false;
    }

    pub fn copy_selection_to_clipboard(&mut self) -> bool {
        if let Some(text) = self.input.selected_text()
            && !text.is_empty()
        {
            let _ = set_clipboard_text(&text);
            return true;
        }
        let max_width = (self.chat_area.width as usize).saturating_sub(1).max(20);
        let lines = crate::ui::views::chat::build_chat_lines(self, max_width);
        if let Some(ref sel) = self.text_selection
            && let Some(text) = crate::ui::views::chat::extract_selected_text(&lines, sel)
            && !text.is_empty()
        {
            let _ = set_clipboard_text(&text);
            return true;
        }
        false
    }

    pub fn start_input_selection(&mut self, char_idx: usize) {
        self.input.selection = Some((char_idx, char_idx));
        self.is_selecting_input = true;
    }

    pub fn update_input_selection(&mut self, char_idx: usize) {
        if let Some((anchor, _)) = self.input.selection {
            self.input.selection = Some((anchor, char_idx));
        } else {
            self.input.selection = Some((char_idx, char_idx));
        }
    }

    pub fn finish_input_selection(&mut self) {
        self.is_selecting_input = false;
        if let Some((anchor, head)) = self.input.selection
            && anchor == head
        {
            self.input.selection = None;
        }
    }

    pub fn clear_input_selection(&mut self) {
        self.is_selecting_input = false;
        self.input.clear_selection();
    }
}

/// 将文本设置到系统剪贴板（支持原生系统剪贴板与 ANSI OSC 52 转义序列）
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    let mut success = false;
    if let Ok(mut clipboard) = arboard::Clipboard::new()
        && clipboard.set_text(text).is_ok()
    {
        success = true;
    }

    // 同时输出 ANSI OSC 52 转义序列，支持远程终端及 SSH 剪贴板透传
    let b64 = base64_encode(text.as_bytes());
    let osc52 = format!("\x1b]52;c;{b64}\x07");
    let mut stdout = std::io::stdout();
    let _ = std::io::Write::write_all(&mut stdout, osc52.as_bytes());
    let _ = std::io::Write::flush(&mut stdout);

    if success {
        Ok(())
    } else {
        Err("Failed to set system clipboard".to_string())
    }
}

/// 从系统剪贴板读取文本
pub fn get_clipboard_text() -> Option<String> {
    if let Ok(mut clipboard) = arboard::Clipboard::new()
        && let Ok(text) = clipboard.get_text()
        && !text.is_empty()
    {
        Some(text)
    } else {
        None
    }
}

pub fn base64_encode(bytes: &[u8]) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut res = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        res.push(CHARSET[(b0 >> 2) as usize] as char);
        res.push(CHARSET[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            res.push(CHARSET[(((b1 & 0xf) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            res.push('=');
        }
        if chunk.len() > 2 {
            res.push(CHARSET[(b2 & 0x3f) as usize] as char);
        } else {
            res.push('=');
        }
    }
    res
}
