use super::FilterableModal;
use crate::ui::input::InputBuffer;

pub const MESSAGE_ACTIONS: [(&str, &str); 2] = [
    ("Revert", "Discard later messages and edit this prompt"),
    ("Fork", "Create a new session from this point"),
];

/// 点击历史用户消息后的 Revert / Fork 选择
#[derive(Debug, Clone)]
pub struct MessageActionState {
    pub turn_index: usize,
    pub prompt: String,
    pub selected_index: usize,
    query: String,
}

impl MessageActionState {
    pub fn new(turn_index: usize, prompt: String) -> Self {
        Self {
            turn_index,
            prompt,
            selected_index: 0,
            query: String::new(),
        }
    }

    pub fn is_fork(&self) -> bool {
        self.selected_index == 1
    }
}

impl FilterableModal for MessageActionState {
    fn filtered_len(&self) -> usize {
        MESSAGE_ACTIONS.len()
    }

    fn selected_index_mut(&mut self) -> &mut usize {
        &mut self.selected_index
    }

    fn query_mut(&mut self) -> &mut String {
        &mut self.query
    }

    fn refilter(&mut self) {}
}

/// Fork 前编辑新会话标题
#[derive(Debug, Clone)]
pub struct ForkTitleState {
    pub turn_index: usize,
    pub prompt: String,
    pub title: InputBuffer,
}

impl ForkTitleState {
    pub fn new(turn_index: usize, prompt: String, title: &str) -> Self {
        let mut input = InputBuffer::new();
        input.set(title);
        Self {
            turn_index,
            prompt,
            title: input,
        }
    }

    pub fn into_message_action(self) -> MessageActionState {
        let mut state = MessageActionState::new(self.turn_index, self.prompt);
        state.selected_index = 1;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_action_navigates_between_revert_and_fork() {
        let mut state = MessageActionState::new(3, "prompt".into());
        assert!(!state.is_fork());
        state.next();
        assert!(state.is_fork());
        state.next();
        assert!(!state.is_fork());
        state.previous();
        assert!(state.is_fork());
    }
}
