pub mod content;
pub mod message;
pub mod meta;
pub mod session;
pub mod tool;
pub mod turn;

pub use content::{Annotation, ContentPart, ImageDetail, MediaSource, SearchStatus, WebSearchCall, WebSearchSource};
pub use message::{Message, Role};
pub use meta::{
    AnthropicMessageMeta, AnthropicSessionMeta, AnthropicTurnMeta, GeminiCodeExecutionResult,
    GeminiMessageMeta, GeminiSafetyRating, GeminiSessionMeta, GeminiTurnMeta, MessageMeta,
    ResponseMessageMeta, ResponseSessionMeta, ResponseTurnMeta, SessionMeta, TurnMeta,
};
pub use session::{JsonlRecord, Session, SessionHeader, SessionSummary};
pub use tool::{ToolCall, ToolDefinition, ToolResult};
pub use turn::{TokenUsage, Turn};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_multimodal_content_serialization() {
        let text_part = ContentPart::text_with_annotations(
            "Hello, according to documentation [1], Rust 2024 is great.",
            vec![Annotation::UrlCitation {
                url: "https://doc.rust-lang.org/edition-guide/rust-2024/index.html".into(),
                title: "Rust 2024 Edition Guide".into(),
                start_index: 34,
                end_index: 37,
            }],
        );

        let image_part = ContentPart::image_url("https://example.com/diag.png", Some(ImageDetail::High));
        let reasoning_part = ContentPart::reasoning("Analyzing the requirements...", Some("claude_sig_12345".into()));

        let parts = vec![text_part, image_part, reasoning_part];
        let json = serde_json::to_string_pretty(&parts).expect("serialize parts");
        let decoded: Vec<ContentPart> = serde_json::from_str(&json).expect("deserialize parts");

        assert_eq!(parts, decoded);
    }

    #[test]
    fn test_typed_metadata_integrity() {
        let mut turn_meta = TurnMeta::default();
        turn_meta.response = Some(ResponseTurnMeta {
            response_id: Some("resp_abc123xyz".into()),
            previous_response_id: Some("resp_root000".into()),
            status: "completed".into(),
            service_tier: Some("default".into()),
            reasoning_effort: Some("high".into()),
            store: true,
        });

        turn_meta.anthropic = Some(AnthropicTurnMeta {
            cache_creation_input_tokens: Some(1024),
            cache_read_input_tokens: Some(512),
            stop_reason: Some("end_turn".into()),
        });

        turn_meta.gemini = Some(GeminiTurnMeta {
            finish_reason: Some("STOP".into()),
            safety_ratings: vec![GeminiSafetyRating {
                category: "HARM_CATEGORY_HATE_SPEECH".into(),
                probability: "NEGLIGIBLE".into(),
            }],
            code_execution_result: Some(GeminiCodeExecutionResult {
                outcome: "OUTCOME_OK".into(),
                output: "Hello from sandbox".into(),
            }),
        });

        let json = serde_json::to_string(&turn_meta).expect("serialize turn_meta");
        let decoded: TurnMeta = serde_json::from_str(&json).expect("deserialize turn_meta");

        assert_eq!(turn_meta, decoded);
        assert_eq!(
            decoded.response.as_ref().unwrap().response_id.as_deref(),
            Some("resp_abc123xyz")
        );
        assert_eq!(
            decoded.anthropic.as_ref().unwrap().cache_read_input_tokens,
            Some(512)
        );
        assert_eq!(
            decoded.gemini.as_ref().unwrap().code_execution_result.as_ref().unwrap().output,
            "Hello from sandbox"
        );
    }

    #[test]
    fn test_web_search_call_roundtrip() {
        let search_call = ContentPart::WebSearchCall(WebSearchCall::completed(
            "ws_123",
            "rust async trait 2024",
            vec![WebSearchSource::new(
                "https://blog.rust-lang.org",
                "Rust Blog",
                Some("Async fn in trait is now stabilized".into()),
            )],
        ));

        let json = serde_json::to_string(&search_call).expect("serialize search call");
        let decoded: ContentPart = serde_json::from_str(&json).expect("deserialize search call");

        assert_eq!(search_call, decoded);
    }

    #[test]
    fn test_session_creation_and_turns() {
        let mut session = Session::new(PathBuf::from("/home/user/project"), Some("Test Task".into()));
        assert_eq!(session.turns.len(), 0);

        let user_msg = Message::user("Hello world");
        let user_turn = Turn::user_turn(user_msg, None);
        session.append_turn(user_turn);

        assert_eq!(session.turns.len(), 1);
        assert!(session.latest_openai_response_id().is_none());

        let mut assistant_turn = Turn::assistant_turn(
            vec![Message::assistant(vec![ContentPart::text("Hi there!")])],
            session.last_turn_id(),
        );
        assistant_turn.meta.response = Some(ResponseTurnMeta::new("resp_001"));
        session.append_turn(assistant_turn);

        assert_eq!(session.turns.len(), 2);
        assert_eq!(session.latest_openai_response_id(), Some("resp_001"));
    }
}

