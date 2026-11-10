use crate::test_support::websocket::*;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as Frame;

use super::ResponseProvider;
use crate::event::AgentEvent;
use crate::model::{ContentPart, Message, Role, Session, ToolCall, ToolResult, Turn};
use crate::provider::{CompletionOptions, Provider, ProviderResponse};

fn append_response(session: &mut Session, response: ProviderResponse) {
    let mut turn = Turn::assistant_turn(response.messages);
    turn.meta = response.meta;
    turn.usage = response.usage;
    session.append_turn(turn);
}

fn options() -> CompletionOptions {
    CompletionOptions {
        model: "m".into(),
        affinity: Some("fixture".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn stateless_continuation_sends_only_new_tool_results_and_user_input() {
    use crate::model::{MediaContent, MediaKind};
    use base64::Engine;

    let mut server = MockWebSocket::new().await;
    let provider = ResponseProvider::new("fixture-key").with_base_url(format!("{}/v1", server.url));
    let mut session = Session::new(".", None);
    session.append_turn(Turn::user_turn(Message::text("Inspect synthetic fixtures")));
    session.append_turn(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::ToolCall(ToolCall::new(
            "read_fixture",
            "Read",
            r#"{"path":"fixture.pdf"}"#,
        )),
    ])]));
    let mut result = ToolResult::success("read_fixture", "Synthetic documents");
    let pdf = format!("%PDF-1.4\n{}\n%%EOF", "% synthetic padding\n".repeat(4096));
    result.attachments = vec![
        MediaContent { kind: MediaKind::Image, mime_type: "image/png".into(), filename: "pixel.png".into(),
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=".into() },
        MediaContent { kind: MediaKind::Pdf, mime_type: "application/pdf".into(), filename: "fixture.pdf".into(),
            data: base64::engine::general_purpose::STANDARD.encode(pdf) },
    ];
    session.append_turn(Turn::new(Role::Tool, vec![Message::tool_result(result)]));
    let opts = options();
    let (response, first) = tokio::join!(provider.complete(&session, &[], &opts), async {
        let request = server.next().await;
        assert_eq!(request.body["type"], "response.create");
        assert!(request.body.get("stream").is_none());
        assert!(request.body.get("previous_response_id").is_none());
        assert_eq!(request.body["input"][2]["output"][1]["type"], "input_image");
        assert_eq!(request.body["input"][2]["output"][2]["type"], "input_file");
        request
            .reply(completed(
                "resp_first",
                vec![json!({
                    "type": "function_call", "id": "fc_next", "call_id": "next",
                    "name": "Read", "arguments": "{}", "status": "completed"
                })],
            ))
            .await;
        request
    });
    let response = response.unwrap();
    let meta = response.meta.response.as_ref().unwrap();
    assert!(!meta.store);
    assert!(meta.response_id.is_none());
    append_response(&mut session, response);
    session.append_turn(Turn::new(
        Role::Tool,
        vec![Message::tool_result(ToolResult::success(
            "next",
            "new result",
        ))],
    ));
    let (response, second) = tokio::join!(provider.complete(&session, &[], &opts), async {
        let request = server.next().await;
        assert_eq!(request.connection, first.connection);
        assert_eq!(request.body["previous_response_id"], "resp_first");
        assert_eq!(
            request.body["input"],
            json!([
                {"type": "function_call_output", "call_id": "next", "output": "new result"}
            ])
        );
        assert!(request.body.to_string().len() < first.body.to_string().len() / 10);
        request.reply(answer("resp_second")).await;
        request
    });
    append_response(&mut session, response.unwrap());

    // 空闲期间仍响应服务端 Ping，不依赖下一次调用驱动控制帧。
    second.send(Frame::Ping(vec![1, 2, 3].into())).await;
    tokio::time::timeout(Duration::from_secs(5), server.pongs.recv())
        .await
        .unwrap()
        .unwrap();

    session.append_turn(Turn::user_turn(Message::text(
        "Continue the synthetic example",
    )));
    let (events, mut received) = mpsc::channel(8);
    let (response, ()) = tokio::join!(
        provider.complete_stream(&session, &[], &opts, Some(events)),
        async {
            let request = server.next().await;
            assert_eq!(request.connection, first.connection);
            assert_eq!(request.body["previous_response_id"], "resp_second");
            assert_eq!(request.body["input"].as_array().unwrap().len(), 1);
            request
                .reply(
                    json!({"type": "response.output_text.delta", "item_id": "msg_fixture",
            "output_index": 0, "content_index": 0, "sequence_number": 1, "delta": "fixture"}),
                )
                .await;
            request.reply(answer("resp_third")).await;
        }
    );
    response.unwrap();
    assert!(
        matches!(received.try_recv().unwrap(), AgentEvent::Text { delta, .. } if delta == "fixture")
    );
}

#[tokio::test]
async fn changed_history_parameters_or_session_starts_a_new_chain() {
    let mut server = MockWebSocket::new().await;
    let provider = ResponseProvider::new("fixture").with_base_url(format!("{}/v1", server.url));
    let mut session = Session::new(".", None);
    session.append_turn(Turn::user_turn(Message::text("initial fixture")));
    let mut opts = options();
    for index in 0..6 {
        match index {
            1 => session.turns[0].messages[0] = Message::text("revised fixture"),
            2 => opts.reasoning_effort = Some("medium".into()),
            3 => opts.model = "another-model".into(),
            4 => opts.affinity = Some("another-scope".into()),
            5 => session.header.id = "sess_other".into(),
            _ => {}
        }
        let (response, ()) = tokio::join!(provider.complete(&session, &[], &opts), async {
            let request = server.next().await;
            assert!(request.body.get("previous_response_id").is_none());
            assert!(!request.body["input"].as_array().unwrap().is_empty());
            request.reply(answer(&format!("resp_{index}"))).await;
        });
        append_response(&mut session, response.unwrap());
    }
}

#[tokio::test]
async fn idle_disconnect_reconnects_with_full_history_and_no_connection_anchor() {
    let mut server = MockWebSocket::new().await;
    let provider = ResponseProvider::new("fixture").with_base_url(format!("{}/v1", server.url));
    let mut session = Session::new(".", None);
    session.append_turn(Turn::user_turn(Message::text("reconnect fixture")));
    let opts = options();
    let (response, first) = tokio::join!(provider.complete(&session, &[], &opts), async {
        let request = server.next().await;
        let mut event = answer("resp_before_close");
        event["response"]["store"] = json!(true);
        request.reply(event).await;
        request
    });
    append_response(&mut session, response.unwrap());
    first.send(Frame::Close(None)).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if provider
                .websocket_session
                .lock()
                .await
                .as_ref()
                .unwrap()
                .is_closed()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (response, ()) = tokio::join!(provider.complete(&session, &[], &opts), async {
        let request = server.next().await;
        assert_ne!(request.connection, first.connection);
        assert!(request.body.get("previous_response_id").is_none());
        assert_eq!(request.body["input"].as_array().unwrap().len(), 2);
        request.reply(answer("resp_new_connection")).await;
    });
    response.unwrap();
}

#[tokio::test]
async fn failed_requests_discard_the_connection_without_automatic_replay() {
    for (frame, expected) in [
        (Frame::Text(json!({"type": "error", "code": "previous_response_not_found", "message": "anchor expired"}).to_string().into()), "anchor expired"),
        (Frame::Text(json!({"type": "response.failed", "response": {"id": "resp_failed", "status": "failed", "error": {"message": "fixture failure"}}}).to_string().into()), "fixture failure"),
        (Frame::Text(json!({"type": "response.incomplete", "response": {"id": "resp_incomplete", "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}}}).to_string().into()), "without response.completed"),
        (Frame::Text("invalid JSON".into()), "Invalid WebSocket JSON"),
        (Frame::Close(None), "without response.completed"),
    ] {
        let mut server = MockWebSocket::new().await;
        let provider = ResponseProvider::new("fixture").with_base_url(format!("{}/v1", server.url));
        let mut session = Session::new(".", None);
        session.append_turn(Turn::user_turn(Message::text("error fixture")));
        let opts = options();
        let (response, ()) = tokio::join!(provider.complete(&session, &[], &opts), async {
            server.next().await.reply(answer("resp_anchor")).await;
        });
        append_response(&mut session, response.unwrap());
        session.append_turn(Turn::user_turn(Message::text("next fixture")));
        let (result, failed) = tokio::join!(provider.complete(&session, &[], &opts), async {
            let request = server.next().await;
            assert_eq!(request.body["previous_response_id"], "resp_anchor");
            request.send(frame).await;
            request
        });
        assert!(result.unwrap_err().to_string().contains(expected));
        assert!(server.requests.try_recv().is_err());
        let (result, ()) = tokio::join!(provider.complete(&session, &[], &opts), async {
            let request = server.next().await;
            assert_ne!(request.connection, failed.connection);
            assert!(request.body.get("previous_response_id").is_none());
            request.reply(answer("resp_recovered")).await;
        });
        result.unwrap();
    }
}

#[tokio::test]
async fn cancelling_a_partial_response_discards_its_connection() {
    let mut server = MockWebSocket::new().await;
    let provider =
        Arc::new(ResponseProvider::new("fixture").with_base_url(format!("{}/v1", server.url)));
    let mut session = Session::new(".", None);
    session.append_turn(Turn::user_turn(Message::text("cancellation fixture")));
    let opts = options();
    let (events, mut received) = mpsc::channel(8);
    let calling = {
        let provider = provider.clone();
        let session = session.clone();
        let opts = opts.clone();
        tokio::spawn(async move {
            provider
                .complete_stream(&session, &[], &opts, Some(events))
                .await
        })
    };
    let cancelled = server.next().await;
    cancelled
        .reply(
            json!({"type": "response.output_text.delta", "item_id": "msg_cancelled",
        "output_index": 0, "content_index": 0, "sequence_number": 1, "delta": "partial fixture"}),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    calling.abort();
    assert!(calling.await.unwrap_err().is_cancelled());
    let (result, ()) = tokio::join!(provider.complete(&session, &[], &opts), async {
        let request = server.next().await;
        assert_ne!(request.connection, cancelled.connection);
        assert!(request.body.get("previous_response_id").is_none());
        request.reply(answer("resp_after_cancel")).await;
    });
    result.unwrap();
}
