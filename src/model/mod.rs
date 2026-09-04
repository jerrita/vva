pub mod content;
pub mod message;
pub mod meta;
pub mod session;
pub mod tool;
pub mod turn;

pub use content::{
    Annotation, ContentPart, ImageDetail, MediaSource, SearchStatus, WebSearchCall, WebSearchSource,
};
pub use message::{Message, Role};
pub use meta::{
    AnthropicMessageMeta, AnthropicSessionMeta, AnthropicTurnMeta, GeminiCodeExecutionResult,
    GeminiMessageMeta, GeminiSafetyRating, GeminiSessionMeta, GeminiTurnMeta, MessageMeta,
    ResponseMessageContent, ResponseMessageMeta, ResponseMessageRole, ResponseOutputItem,
    ResponseReasoningContent, ResponseReasoningSummary, ResponseSessionMeta, ResponseTurnMeta,
    ResponseWebSearchAction, ResponseWebSearchSource, ResponseWebSearchSourceType, SessionMeta,
    TurnMeta,
};
pub use session::{JsonlRecord, Session, SessionHeader, SessionSummary};
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

        let user_turn = Turn::user_turn(Message::user("Hello world"), None);
        session.append_turn(user_turn);
        assert!(session.latest_openai_response_id().is_none());

        let mut assistant_turn = Turn::assistant_turn(
            vec![Message::assistant(vec![ContentPart::text("Hi there!")])],
            session.last_turn_id(),
        );
        assistant_turn.meta.response = Some(ResponseTurnMeta::new("resp_001"));
        session.append_turn(assistant_turn);

        assert_eq!(session.latest_openai_response_id(), Some("resp_001"));
    }
}
