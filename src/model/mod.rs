pub mod content;
pub mod message;
pub mod meta;
pub mod session;
pub mod tool;
pub mod turn;

pub use content::{
    Annotation, ContentPart, ImageGenerationCall, ImageGenerationStatus, MediaSource, SearchStatus,
    WebSearchAction, WebSearchCall, WebSearchSource, format_file_ref,
};
pub use message::{Message, Role};
pub use meta::{
    AnthropicCitation, AnthropicContentBlock, AnthropicDocumentBlock, AnthropicDocumentSource,
    AnthropicFetchResult, AnthropicFetchResultContent, AnthropicSearchResult,
    AnthropicSearchResultContent, AnthropicToolResultError, AnthropicTurnMeta,
    GeminiCodeExecutionResult, GeminiSafetyRating, GeminiTurnMeta, ResponseMessageContent,
    ResponseMessageRole, ResponseOutputItem, ResponseReasoningContent, ResponseReasoningSummary,
    ResponseTurnMeta, ResponseWebSearchAction, ResponseWebSearchSource,
    ResponseWebSearchSourceType, TurnMeta,
};
pub use session::{
    EditorKind, JsonlRecord, Session, SessionHeader, SessionSummary, generate_session_id,
    provisional_title,
};
pub use tool::{
    MediaContent, MediaKind, ToolCall, ToolDefinition, ToolResult, UserAnswer, UserQuestion,
    UserQuestionOption, UserQuestionRequest,
};
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
        assert!(session.latest_openai_response_id(None).is_none());

        let mut assistant_turn =
            Turn::assistant_turn(vec![Message::new(vec![ContentPart::text("Hi there!")])]);
        assistant_turn.meta.response = Some(ResponseTurnMeta::new("resp_001"));
        assistant_turn.meta.response.as_mut().unwrap().scope = Some("gpt".into());
        session.append_turn(assistant_turn);

        // 亲和组一致可复用锚点；不一致（切换模型）时锚点不可见，回退全量重建
        assert_eq!(
            session.latest_openai_response_id(Some("gpt")),
            Some("resp_001")
        );
        assert_eq!(session.latest_openai_response_id(Some("deepseek")), None);
        assert_eq!(session.latest_openai_response_id(None), None);
    }
}
