use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::model::{ToolCall, UserAnswer, UserQuestion, UserQuestionOption, UserQuestionRequest};

pub(crate) struct AskUserRequest {
    pub prompt: UserQuestionRequest,
    pub response: oneshot::Sender<Result<String, String>>,
}

pub struct AskUserTool {
    requests: mpsc::Sender<AskUserRequest>,
}

impl AskUserTool {
    pub(crate) fn new(requests: mpsc::Sender<AskUserRequest>) -> Self {
        Self { requests }
    }

    fn parse(call: &ToolCall) -> Result<UserQuestionRequest, String> {
        #[derive(Deserialize)]
        struct Arguments {
            questions: Vec<QuestionArguments>,
        }
        #[derive(Deserialize)]
        struct QuestionArguments {
            #[serde(default)]
            header: String,
            question: String,
            options: Vec<OptionArguments>,
            #[serde(default)]
            multi_select: bool,
        }
        #[derive(Deserialize)]
        struct OptionArguments {
            label: String,
            description: String,
        }

        let arguments: Arguments = super::parse_args(&call.arguments, "AskUser")?;
        if arguments.questions.is_empty() {
            return Err("AskUser requires at least one question".into());
        }
        let questions = arguments
            .questions
            .into_iter()
            .map(|question| {
                if question.question.trim().is_empty() {
                    return Err("AskUser question must not be empty".into());
                }
                if question.options.is_empty() {
                    return Err("AskUser question requires at least one option".into());
                }
                Ok(UserQuestion {
                    header: question.header,
                    question: question.question,
                    options: question
                        .options
                        .into_iter()
                        .map(|option| UserQuestionOption {
                            label: option.label,
                            description: option.description,
                        })
                        .collect(),
                    multi_select: question.multi_select,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(UserQuestionRequest {
            id: call.id.clone(),
            questions,
        })
    }
}

#[async_trait]
impl crate::tools::Tool for AskUserTool {
    fn name(&self) -> &str {
        "AskUser"
    }

    fn description(&self) -> &str {
        "Ask the user one or more questions and wait for their answers."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": {"type": "string"},
                            "question": {"type": "string"},
                            "options": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {"type": "string"},
                                        "description": {"type": "string"}
                                    },
                                    "required": ["label", "description"]
                                }
                            },
                            "multi_select": {"type": "boolean"}
                        },
                        "required": ["question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn execute(&self, _arguments: &str) -> Result<String, String> {
        Err("AskUser requires a tool call context".into())
    }

    async fn execute_call(&self, call: &ToolCall) -> Result<String, String> {
        let prompt = Self::parse(call)?;
        let (response, receiver) = oneshot::channel();
        self.requests
            .send(AskUserRequest { prompt, response })
            .await
            .map_err(|_| "Session is no longer accepting user questions".to_string())?;
        receiver
            .await
            .map_err(|_| "User question was canceled".to_string())?
    }
}

pub(crate) fn encode_answers(
    prompt: &UserQuestionRequest,
    answers: &[UserAnswer],
) -> Result<String, String> {
    if answers.len() != prompt.questions.len() {
        return Err("Answers must include every question".into());
    }
    for question in &prompt.questions {
        let answer = answers
            .iter()
            .find(|answer| answer.question == question.question)
            .ok_or_else(|| format!("Missing answer for question: {}", question.question))?;
        if answer.answers.is_empty() {
            return Err(format!("Answer must not be empty: {}", question.question));
        }
        if !question.multi_select && answer.answers.len() != 1 {
            return Err(format!(
                "Question allows only one answer: {}",
                question.question
            ));
        }
    }
    if answers.iter().any(|answer| {
        !prompt
            .questions
            .iter()
            .any(|question| question.question == answer.question)
    }) {
        return Err("Answer contains an unknown question".into());
    }
    serde_json::to_string(&serde_json::json!({"answers": answers}))
        .map_err(|error| format!("Failed to encode AskUser result: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    fn prompt() -> UserQuestionRequest {
        UserQuestionRequest {
            id: "call_1".into(),
            questions: vec![UserQuestion {
                header: "Choice".into(),
                question: "Which editor?".into(),
                options: vec![UserQuestionOption {
                    label: "vim".into(),
                    description: "Terminal editor".into(),
                }],
                multi_select: false,
            }],
        }
    }

    #[test]
    fn encode_answers_requires_one_answer_per_question() {
        let request = prompt();
        let result = encode_answers(
            &request,
            &[UserAnswer {
                question: "Which editor?".into(),
                answers: vec!["vim".into()],
            }],
        )
        .unwrap();
        assert_eq!(
            result,
            r#"{"answers":[{"answers":["vim"],"question":"Which editor?"}]}"#
        );
        assert!(encode_answers(&request, &[]).is_err());
    }

    #[tokio::test]
    async fn tool_waits_for_session_answer() {
        let (sender, mut receiver) = mpsc::channel(1);
        let tool = AskUserTool::new(sender);
        let call = ToolCall::new(
            "call_1",
            "AskUser",
            r#"{"questions":[{"question":"Which editor?","options":[{"label":"vim","description":"Terminal editor"}]}]}"#,
        );
        let task = tokio::spawn(async move { tool.execute_result(&call).await });
        let request = receiver.recv().await.unwrap();
        assert_eq!(request.prompt.id, "call_1");
        let _ = request.response.send(Ok("done".into()));
        assert_eq!(task.await.unwrap().content, "done");
    }
}
