pub mod content;
pub mod message;
pub mod meta;
pub mod session;
pub mod tool;
pub mod turn;

pub use content::{
    Annotation, ContentPart, MediaSource, SearchStatus, WebSearchCall, WebSearchSource,
};
pub use message::{Message, Role};
pub use meta::{
    AnthropicTurnMeta, GeminiCodeExecutionResult, GeminiSafetyRating, GeminiTurnMeta,
    ResponseMessageContent, ResponseMessageRole, ResponseOutputItem, ResponseReasoningContent,
    ResponseReasoningSummary, ResponseTurnMeta, ResponseWebSearchAction, ResponseWebSearchSource,
    ResponseWebSearchSourceType, TurnMeta,
};
pub use session::{JsonlRecord, Session, SessionHeader, SessionSummary, provisional_title};
pub use tool::{ToolCall, ToolDefinition, ToolResult};
pub use turn::{TokenUsage, Turn};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_latest_openai_response_id_scan() {
        let mut session = Session::new(
            PathBuf::from("/home/user/project"),
            Some("Test Task".into()),
        );

        let user_turn = Turn::user_turn(Message::text("Hello world"));
        session.append_turn(user_turn);
        assert!(session.latest_openai_response_id().is_none());

        let mut assistant_turn =
            Turn::assistant_turn(vec![Message::new(vec![ContentPart::text("Hi there!")])]);
        assistant_turn.meta.response = Some(ResponseTurnMeta::new("resp_001"));
        session.append_turn(assistant_turn);

        assert_eq!(session.latest_openai_response_id(), Some("resp_001"));
    }
}
