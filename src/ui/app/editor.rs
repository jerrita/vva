use super::App;
use super::selection::get_clipboard_text;
use crate::ui::command::{FILE_COMPLETION_LIMIT, FileCandidate, match_candidates};

impl App {
    /// 接收后台索引并刷新当前 token 的候选；返回是否需要重绘。
    pub fn poll_file_index(&mut self) -> bool {
        if self.file_index.is_none()
            && let Some(rx) = &self.file_index_rx
            && let Ok(index) = rx.try_recv()
        {
            self.file_index = Some(index);
            self.file_index_rx = None;
            self.update_file_matches();
            return true;
        }
        false
    }

    pub fn at_token_at_cursor(&self) -> Option<(usize, String)> {
        crate::harness::mention::mention_at_cursor(&self.input.text, self.input.cursor_pos)
    }

    pub fn insert_char(&mut self, c: char) {
        self.history_index = None;
        self.file_completion_dismissed = false;
        self.input.insert_char(c);
        self.last_key_input_time = Some(std::time::Instant::now());
        self.update_command_matches();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.history_index = None;
        self.file_completion_dismissed = false;
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
        self.file_completion_dismissed = false;
        self.input.backspace();
        self.update_command_matches();
    }

    pub fn delete(&mut self) {
        self.history_index = None;
        self.file_completion_dismissed = false;
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
        self.file_completion_dismissed = false;
        self.input.set(s);
        self.update_command_matches();
    }

    pub fn clear_input(&mut self) {
        self.clear_input_selection();
        self.file_completion_dismissed = false;
        self.input.clear();
        self.dismiss_completion();
        self.dismiss_file_matches();
    }

    /// 关闭命令补全选单并复位选中项
    pub fn dismiss_completion(&mut self) {
        self.matched_candidates.clear();
        self.selected_command_index = 0;
    }

    /// 关闭 @ 文件补全浮层；同 token 下不再弹出，直至输入再次编辑
    pub fn dismiss_file_completion(&mut self) {
        self.file_completion_dismissed = true;
        self.dismiss_file_matches();
    }

    fn dismiss_file_matches(&mut self) {
        self.file_matches.clear();
        self.selected_file_index = 0;
    }

    pub fn update_command_matches(&mut self) {
        self.poll_file_index();
        self.matched_candidates = match_candidates(&self.input, &self.skills);
        if self.selected_command_index >= self.matched_candidates.len() {
            self.selected_command_index = 0;
        }
        if !self.matched_candidates.is_empty() {
            self.dismiss_file_matches();
            return;
        }
        self.update_file_matches();
    }

    /// 依据光标处的 @ token 刷新文件补全候选；无 token 或已关闭时清空
    fn update_file_matches(&mut self) {
        if self.file_completion_dismissed {
            self.dismiss_file_matches();
            return;
        }
        let Some((_, query)) = self.at_token_at_cursor() else {
            self.dismiss_file_matches();
            return;
        };
        let index = self.file_index.clone().unwrap_or_default();
        let (matches, _) = crate::find::rank_candidates(&index, &query, FILE_COMPLETION_LIMIT);
        self.file_matches = matches
            .into_iter()
            .map(|matched| FileCandidate {
                path: matched.candidate.path.clone(),
                indices: matched.indices,
            })
            .collect();
        if self.selected_file_index >= self.file_matches.len() {
            self.selected_file_index = 0;
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

    pub fn file_select_previous(&mut self) {
        let len = self.file_matches.len();
        if len > 0 {
            self.selected_file_index = (self.selected_file_index + len - 1) % len;
        }
    }

    pub fn file_select_next(&mut self) {
        if !self.file_matches.is_empty() {
            self.selected_file_index = (self.selected_file_index + 1) % self.file_matches.len();
        }
    }

    /// 将选中的 @ 候选插入输入框：以 `@path ` 替换 '@' 起至光标的 token，随后刷新两套补全
    pub fn apply_selected_file(&mut self) {
        let Some((at, _)) = self.at_token_at_cursor() else {
            self.dismiss_file_matches();
            return;
        };
        let Some(candidate) = self.file_matches.get(self.selected_file_index) else {
            return;
        };
        let path = candidate.path.clone();
        self.input.selection = Some((at, self.input.cursor_pos));
        self.input.insert_str(&format!("@{path} "));
        self.update_command_matches();
    }
}
