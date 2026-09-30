use base64::Engine;
use unicode_width::UnicodeWidthChar;

use super::App;

/// 文本区域中的绝对行列坐标
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCoord {
    pub line: usize,
    pub col: usize,
}

/// 鼠标拖拽文本选区
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: TextCoord,
    pub head: TextCoord,
}

/// 选区状态追踪器：选区与其"拖拽进行中"标志成对维护，消除双字段不变量
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SelectionTracker {
    pub selection: Option<TextSelection>,
    selecting: bool,
}

impl SelectionTracker {
    pub fn start(&mut self, line: usize, col: usize) {
        let coord = TextCoord { line, col };
        self.selection = Some(TextSelection::new(coord, coord));
        self.selecting = true;
    }

    pub fn update(&mut self, line: usize, col: usize) {
        if let Some(ref mut sel) = self.selection {
            sel.head = TextCoord { line, col };
        }
    }

    pub fn finish(&mut self) {
        self.selecting = false;
        if self.selection.as_ref().is_some_and(|sel| sel.is_empty()) {
            self.selection = None;
        }
    }

    pub fn clear(&mut self) {
        self.selection = None;
        self.selecting = false;
    }

    pub fn get(&self) -> Option<&TextSelection> {
        self.selection.as_ref()
    }

    pub fn is_some(&self) -> bool {
        self.selection.is_some()
    }

    pub fn is_selecting(&self) -> bool {
        self.selecting
    }
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

    /// 指定行落在选区内的列区间 (start_col, end_col)；不在选区内或区间为空时返回 None。
    /// 首行从 start.col 起、尾行至 end.col 止、中间行为整行。
    pub fn line_column_range(&self, line_idx: usize) -> Option<(usize, usize)> {
        let (start, end) = self.normalized();
        if line_idx < start.line || line_idx > end.line {
            return None;
        }
        let range = if start.line == end.line {
            (start.col, end.col)
        } else if line_idx == start.line {
            (start.col, usize::MAX)
        } else if line_idx == end.line {
            (0, end.col)
        } else {
            (0, usize::MAX)
        };
        (range.0 < range.1).then_some(range)
    }
}

/// 字符在列区间内的命中判定（按显示列坐标；零宽字符以起始列判定）
pub fn char_in_column_range(ch: char, col: usize, range: (usize, usize)) -> bool {
    let width = ch.width().unwrap_or(0);
    if width == 0 {
        col >= range.0 && col < range.1
    } else {
        col + width > range.0 && col < range.1
    }
}

impl App {
    /// 按级联顺序（输入框 → 聊天流 → 侧边栏）返回首个存在的选区文本
    pub fn active_selection_text(&self) -> Option<String> {
        self.input
            .selected_text()
            .or_else(|| self.chat.selected_text())
            .or_else(|| self.sidebar.selected_text())
            .filter(|text| !text.is_empty())
    }

    /// 复制首个存在的选区到剪贴板并清除该选区，无选区时返回 false
    pub fn copy_active_selection(&mut self) -> bool {
        let Some(text) = self.active_selection_text() else {
            return false;
        };
        let _ = set_clipboard_text(&text);
        self.clear_selections();
        true
    }

    pub fn clear_selections(&mut self) {
        self.clear_input_selection();
        self.chat.selection.clear();
        self.sidebar.selection.clear();
    }

    /// 结束所有进行中的拖拽选区
    pub fn finish_selections(&mut self) {
        self.finish_input_selection();
        self.chat.selection.finish();
        self.sidebar.selection.finish();
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
    let success =
        arboard::Clipboard::new().is_ok_and(|mut clipboard| clipboard.set_text(text).is_ok());

    // 同时输出 ANSI OSC 52 转义序列，支持远程终端及 SSH 剪贴板透传
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let osc52 = format!("\x1b]52;c;{b64}\x07");
    let mut stdout = std::io::stdout();
    let _ = std::io::Write::write_all(&mut stdout, osc52.as_bytes());
    let _ = std::io::Write::flush(&mut stdout);

    success
        .then_some(())
        .ok_or_else(|| "Failed to set system clipboard".to_string())
}

/// 从系统剪贴板读取文本
pub fn get_clipboard_text() -> Option<String> {
    arboard::Clipboard::new()
        .ok()?
        .get_text()
        .ok()
        .filter(|text| !text.is_empty())
}
