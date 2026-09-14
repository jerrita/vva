use crate::model::{UserAnswer, UserQuestionRequest};

#[derive(Debug, Clone)]
pub struct AskUserModalState {
    pub request: UserQuestionRequest,
    pub question_index: usize,
    pub option_index: usize,
    selected: Vec<Vec<bool>>,
}

impl AskUserModalState {
    pub fn new(request: UserQuestionRequest) -> Self {
        let selected = request
            .questions
            .iter()
            .map(|question| vec![false; question.options.len()])
            .collect();
        Self {
            request,
            question_index: 0,
            option_index: 0,
            selected,
        }
    }

    pub fn current_question(&self) -> &crate::model::UserQuestion {
        &self.request.questions[self.question_index]
    }

    pub fn next_option(&mut self) {
        let len = self.current_question().options.len();
        if len > 0 {
            self.option_index = (self.option_index + 1) % len;
        }
    }

    pub fn previous_option(&mut self) {
        let len = self.current_question().options.len();
        if len > 0 {
            self.option_index = if self.option_index == 0 {
                len - 1
            } else {
                self.option_index - 1
            };
        }
    }

    pub fn toggle_option(&mut self) {
        let question = self.current_question();
        if question.multi_select {
            self.selected[self.question_index][self.option_index] =
                !self.selected[self.question_index][self.option_index];
        } else {
            self.selected[self.question_index].fill(false);
            self.selected[self.question_index][self.option_index] = true;
        }
    }

    pub fn selected_option(&self, index: usize) -> bool {
        self.selected[self.question_index][index]
    }

    pub fn advance(&mut self) -> bool {
        if !self.current_question().multi_select
            && !self.selected[self.question_index]
                .iter()
                .any(|selected| *selected)
        {
            self.toggle_option();
        }
        if self.question_index + 1 < self.request.questions.len() {
            self.question_index += 1;
            self.option_index = 0;
            false
        } else {
            true
        }
    }

    pub fn answers(&self) -> Option<Vec<UserAnswer>> {
        let mut answers = Vec::with_capacity(self.request.questions.len());
        for (question, selected) in self.request.questions.iter().zip(&self.selected) {
            let values: Vec<_> = question
                .options
                .iter()
                .zip(selected)
                .filter(|(_, selected)| **selected)
                .map(|(option, _)| option.label.clone())
                .collect();
            if values.is_empty() {
                return None;
            }
            answers.push(UserAnswer {
                question: question.question.clone(),
                answers: values,
            });
        }
        Some(answers)
    }
}
