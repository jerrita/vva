use crate::ui::command::match_commands;
use super::selection::get_clipboard_text;
use super::App;

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
    pub fn paste_from_clipboard(&mut self) -> bool {
        if let Some(text) = get_clipboard_text() {
            self.insert_str(&text);
            true
        } else {
            false
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

    pub fn set_cursor_from_mouse(&mut self, target_row: usize, target_visual_col: usize) -> usize {
        self.input.set_cursor_from_visual_coord(target_row, target_visual_col)
    }

    /// 向上轮换历史 Prompt
    pub fn history_previous(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }

        match self.history_index {
            None => {
                self.saved_draft = self.input.text.clone();
                let last_idx = self.prompt_history.len() - 1;
                self.history_index = Some(last_idx);
                let text = self.prompt_history[last_idx].clone();
                self.set_input(&text);
            }
            Some(idx) if idx > 0 => {
                let prev_idx = idx - 1;
                self.history_index = Some(prev_idx);
                let text = self.prompt_history[prev_idx].clone();
                self.set_input(&text);
            }
            Some(_) => {}
        }
    }

    /// 向下轮换历史 Prompt
    pub fn history_next(&mut self) {
        if let Some(idx) = self.history_index {
            if idx + 1 < self.prompt_history.len() {
                let next_idx = idx + 1;
                self.history_index = Some(next_idx);
                let text = self.prompt_history[next_idx].clone();
                self.set_input(&text);
            } else {
                self.history_index = None;
                let draft = std::mem::take(&mut self.saved_draft);
                self.set_input(&draft);
            }
        }
    }

    pub fn move_cursor_left(&mut self) {
        self.input.move_cursor_left();
    }

    pub fn move_cursor_right(&mut self) {
        self.input.move_cursor_right();
    }

    pub fn move_cursor_up(&mut self) {
        self.input.move_cursor_up();
    }

    pub fn move_cursor_down(&mut self) {
        self.input.move_cursor_down();
    }

    pub fn move_cursor_home(&mut self) {
        self.input.move_cursor_home();
    }

    pub fn move_cursor_end(&mut self) {
        self.input.move_cursor_end();
    }

    pub fn input_line_count(&self) -> usize {
        self.input.line_count()
    }

    pub fn cursor_line_and_col(&self) -> (usize, usize) {
        self.input.cursor_line_and_col()
    }

    pub fn set_input(&mut self, s: &str) {
        self.input.set(s);
        self.update_command_matches();
    }

    pub fn clear_input(&mut self) {
        self.clear_input_selection();
        self.input.clear();
        self.matched_commands.clear();
        self.selected_command_index = 0;
    }

    pub fn update_command_matches(&mut self) {
        if self.input.trim_start().starts_with('/') {
            self.matched_commands = match_commands(&self.input);
            if self.selected_command_index >= self.matched_commands.len() {
                self.selected_command_index = 0;
            }
        } else {
            self.matched_commands.clear();
            self.selected_command_index = 0;
        }
    }

    pub fn command_select_previous(&mut self) {
        if !self.matched_commands.is_empty() {
            self.selected_command_index = if self.selected_command_index == 0 {
                self.matched_commands.len() - 1
            } else {
                self.selected_command_index - 1
            };
        }
    }

    pub fn command_select_next(&mut self) {
        if !self.matched_commands.is_empty() {
            self.selected_command_index =
                (self.selected_command_index + 1) % self.matched_commands.len();
        }
    }

    pub fn complete_selected_command(&mut self) {
        if let Some(cmd) = self.matched_commands.get(self.selected_command_index) {
            self.input.set(&format!("{} ", cmd.name));
            self.update_command_matches();
        }
    }
}
