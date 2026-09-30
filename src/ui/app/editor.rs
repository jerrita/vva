use super::App;
use super::selection::get_clipboard_text;
use crate::ui::command::match_candidates;

impl App {
    pub fn insert_char(&mut self, c: char) {
        self.history_index = None;
        self.input.insert_char(c);
        self.last_key_input_time = Some(std::time::Instant::now());
        self.update_command_matches();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.history_index = None;
        self.input.insert_str(s);
        self.last_key_input_time = Some(std::time::Instant::now());
        self.update_command_matches();
    }

    /// 从系统剪贴板粘贴文本到当前输入框 (保留多行格式，不提前提交)
    pub fn paste_from_clipboard(&mut self) {
        if let Some(text) = get_clipboard_text() {
            self.insert_str(&text);
        }
    }

    pub fn backspace(&mut self) {
        self.history_index = None;
        self.input.backspace();
        self.update_command_matches();
    }

    pub fn delete(&mut self) {
        self.history_index = None;
        self.input.delete();
        self.update_command_matches();
    }

    /// 向上轮换历史 Prompt
    pub fn history_previous(&mut self) {
        let index = match self.history_index {
            None if self.prompt_history.is_empty() => return,
            None => {
                self.saved_draft = self.input.text.clone();
                self.prompt_history.len() - 1
            }
            Some(0) => return,
            Some(idx) => idx - 1,
        };
        self.show_history(index);
    }

    /// 向下轮换历史 Prompt；越过最新一条时恢复草稿
    pub fn history_next(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };
        if idx + 1 < self.prompt_history.len() {
            self.show_history(idx + 1);
        } else {
            self.history_index = None;
            let draft = std::mem::take(&mut self.saved_draft);
            self.set_input(&draft);
        }
    }

    fn show_history(&mut self, index: usize) {
        self.history_index = Some(index);
        let text = self.prompt_history[index].clone();
        self.set_input(&text);
    }

    pub fn set_input(&mut self, s: &str) {
        self.input.set(s);
        self.update_command_matches();
    }

    pub fn clear_input(&mut self) {
        self.clear_input_selection();
        self.input.clear();
        self.dismiss_completion();
    }

    /// 关闭命令补全选单并复位选中项
    pub fn dismiss_completion(&mut self) {
        self.matched_candidates.clear();
        self.selected_command_index = 0;
    }

    pub fn update_command_matches(&mut self) {
        if self.input.trim_start().starts_with('/') {
            self.matched_candidates = match_candidates(&self.input, &self.skills);
            if self.selected_command_index >= self.matched_candidates.len() {
                self.selected_command_index = 0;
            }
        } else {
            self.dismiss_completion();
        }
    }

    pub fn command_select_previous(&mut self) {
        let len = self.matched_candidates.len();
        if len > 0 {
            self.selected_command_index = (self.selected_command_index + len - 1) % len;
        }
    }

    pub fn command_select_next(&mut self) {
        if !self.matched_candidates.is_empty() {
            self.selected_command_index =
                (self.selected_command_index + 1) % self.matched_candidates.len();
        }
    }

    pub fn complete_selected_command(&mut self) {
        if let Some(candidate) = self.matched_candidates.get(self.selected_command_index) {
            self.input.set(&format!("{} ", candidate.token()));
            self.update_command_matches();
        }
    }
}
