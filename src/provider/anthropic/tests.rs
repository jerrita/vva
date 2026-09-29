use std::collections::HashMap;

use crate::event::AgentEvent;
use crate::model::{
    Annotation, ContentPart, Message, Role, SearchStatus, Session, ToolCall, WebSearchAction,
};
use crate::provider::common::EventForwarder;
use crate::provider::http::SseEventHandler;
use crate::provider::{CompletionOptions, ProviderError};
use tokio::sync::mpsc;

use super::parse::*;
use super::stream::*;
use super::*;

use crate::model::*;
use std::path::PathBuf;

#[test]
fn media_tool_results_use_native_content_blocks() {
    let session = super::super::common::media_tool_session();
    let body = AnthropicProvider::new("test")
        .build_request_body(&session, &[], &CompletionOptions::default())
        .unwrap();
    let output = &body["messages"][1]["content"][0];
    assert_eq!(output["tool_use_id"], "read_media");
    assert_eq!(output["content"][0]["type"], "text");
    assert_eq!(output["content"][1]["type"], "image");
    assert_eq!(output["content"][1]["source"]["media_type"], "image/png");
    assert!(
        output["content"][1]["source"]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBOR")
    );
}

#[test]
fn test_anthropic_thinking_signature_and_caching_roundtrip() {
    let provider = AnthropicProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);

    // 用户输入
    let user_msg = Message::text("Solve this puzzle with deep thinking");
    session.append_turn(Turn::user_turn(user_msg));

    // 模拟 Anthropic 返回包含 thinking block 和 signature 的响应
    let mock_resp = serde_json::json!({
        "id": "msg_01X999",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "thinking",
                "thinking": "Step 1: analyze conditions. Step 2: deduce solution.",
                "signature": "sig_cryptographic_verification_token"
            },
            {
                "type": "text",
                "text": "The answer is 42."
            }
        ],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 1200,
            "output_tokens": 350,
            "cache_creation_input_tokens": 1000,
            "cache_read_input_tokens": 0
        }
    });

    let parsed = provider.parse_response(mock_resp).expect("parse response");
    let anthropic_meta = parsed.meta.anthropic.as_ref().unwrap();
    assert_eq!(anthropic_meta.cache_creation_input_tokens, Some(1000));
    assert_eq!(anthropic_meta.stop_reason.as_deref(), Some("end_turn"));

    // 将 assistant turn 加入 session
    let mut assistant_turn = Turn::assistant_turn(parsed.messages);
    assistant_turn.meta = parsed.meta;
    session.append_turn(assistant_turn);

    // 下一轮用户继续追问
    session.append_turn(Turn::user_turn(Message::text("Why 42?")));

    // 生成下一次调用的 request body，验证 thinking 和 signature 被完整保留
    let options = CompletionOptions {
        model: "claude-3-7-sonnet-20250219".into(),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);

    // 检查第二条（assistant 历史）中是否保留了 signature
    let asst_content = msgs[1]["content"].as_array().unwrap();
    assert_eq!(asst_content[0]["type"], "thinking");
    assert_eq!(
        asst_content[0]["signature"],
        "sig_cryptographic_verification_token"
    );

    // 检查最后一条用户消息是否自动注入了 cache_control
    let last_user_content = msgs[2]["content"].as_array().unwrap();
    assert_eq!(last_user_content[0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn test_anthropic_tool_results_merged_into_single_user_turn() {
    let provider =
        AnthropicProvider::new("test-key").with_base_url("https://proxy.example.com/v1/messages/");
    let mut session = Session::new(PathBuf::from("/tmp"), None);

    session.append_turn(Turn::user_turn(Message::text("List files and run command")));

    let asst_msg = Message::new(vec![
        ContentPart::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "file_view".into(),
            arguments: "{}".into(),
        }),
        ContentPart::ToolCall(ToolCall {
            id: "call_2".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        }),
    ]);
    session.append_turn(Turn::assistant_turn(vec![asst_msg]));

    // 两个连续的 Tool turn
    session.append_turn(Turn::new(
        Role::Tool,
        vec![Message::tool_result(ToolResult {
            background_task_id: None,
            attachments: Vec::new(),
            tool_call_id: "call_1".into(),
            content: "file contents".into(),
            is_error: false,
        })],
    ));
    session.append_turn(Turn::new(
        Role::Tool,
        vec![Message::tool_result(ToolResult {
            background_task_id: None,
            attachments: Vec::new(),
            tool_call_id: "call_2".into(),
            content: "command output".into(),
            is_error: false,
        })],
    ));

    let options = CompletionOptions {
        model: "claude-3-7-sonnet".into(),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");
    let msgs = body["messages"].as_array().unwrap();

    // 验证两个 tool result 被合并到了同一个 user message 中，保证严格交替：user -> assistant -> user
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[2]["role"], "user");
    let tool_blocks = msgs[2]["content"].as_array().unwrap();
    assert_eq!(tool_blocks.len(), 2);
    assert_eq!(tool_blocks[0]["type"], "tool_result");
    assert_eq!(tool_blocks[1]["type"], "tool_result");
}

/// 流式增量累积与非流式解析必须产出同一结果（一个解析器、两个入口）
#[tokio::test]
async fn test_anthropic_streaming_matches_non_streaming_parse() {
    let provider = AnthropicProvider::new("test-key");
    let full = serde_json::json!({
        "id": "msg_stream_1",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "thinking", "thinking": "Step 1.", "signature": "sig_abc" },
            { "type": "text", "text": "Answer body." },
            { "type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"command": "ls"} }
        ],
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": 100,
            "output_tokens": 40,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 80
        }
    });

    let parsed = provider.parse_response(full.clone()).expect("parse full");

    // 用等价的流式事件序列驱动同一个组装路径
    let mut handler = AnthropicStreamHandler {
        blocks: Vec::new(),
        partial_inputs: HashMap::new(),
        usage: AnthropicUsageSnapshot::default(),
        forwarder: EventForwarder::new(None),
    };
    let events = [
        serde_json::json!({
            "type": "message_start",
            "message": {
                "usage": {
                    "input_tokens": 100,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 80
                }
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "thinking", "thinking": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "Step 1." }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "signature_delta", "signature": "sig_abc" }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 1,
            "content_block": { "type": "text", "text": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 1,
            "delta": { "type": "text_delta", "text": "Answer body." }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 2,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "bash", "input": {} }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 2,
            "delta": { "type": "input_json_delta", "partial_json": "{\"command\":\"ls\"}" }
        }),
        serde_json::json!({
            "type": "content_block_stop", "index": 2
        }),
        serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "tool_use" },
            "usage": { "output_tokens": 40 }
        }),
        serde_json::json!({ "type": "message_stop" }),
    ];
    for ev in events {
        handler.on_event(ev).await.expect("handle stream event");
    }
    let (blocks, usage) = handler.finish().expect("finish");
    let streamed = assemble_response(blocks, usage).expect("assemble");

    assert_eq!(streamed.messages[0].parts, parsed.messages[0].parts);
    assert_eq!(streamed.usage, parsed.usage);
    assert_eq!(streamed.meta.anthropic, parsed.meta.anthropic);
}

#[tokio::test]
async fn test_anthropic_stream_error_event_reports_protocol_error() {
    let mut handler = AnthropicStreamHandler {
        blocks: Vec::new(),
        partial_inputs: HashMap::new(),
        usage: AnthropicUsageSnapshot::default(),
        forwarder: EventForwarder::new(None),
    };
    let err = handler
        .on_event(serde_json::json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" }
        }))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProviderError::Protocol(ref msg) if msg.contains("Overloaded")),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn test_anthropic_stream_unknown_block_type_reports_protocol_error() {
    let mut handler = AnthropicStreamHandler {
        blocks: Vec::new(),
        partial_inputs: HashMap::new(),
        usage: AnthropicUsageSnapshot::default(),
        forwarder: EventForwarder::new(None),
    };
    let err = handler
        .on_event(serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "future_block", "id": "x", "name": "y" }
        }))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProviderError::Protocol(ref msg) if msg.contains("future_block")),
        "unexpected error: {err:?}"
    );
}

/// 官方 web search 响应样例：server_tool_use 与结果块配对投影为 WebSearchCall，
/// 引用注脚投影为 WebSearchResultLocation，原生块链逐块保真回传
#[test]
fn test_anthropic_web_search_parse_and_native_echo() {
    let provider = AnthropicProvider::new("test-key");
    let mock_resp = serde_json::json!({
        "id": "msg_ws_1",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "text", "text": "I'll search for when Claude Shannon was born." },
            {
                "type": "server_tool_use",
                "id": "srvtoolu_01WYG3ziw53XMcoyKL4XcZmE",
                "name": "web_search",
                "input": { "query": "claude shannon birth date" }
            },
            {
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_01WYG3ziw53XMcoyKL4XcZmE",
                "content": [
                    {
                        "type": "web_search_result",
                        "url": "https://en.wikipedia.org/wiki/Claude_Shannon",
                        "title": "Claude Shannon - Wikipedia",
                        "encrypted_content": "EqgfCioIARgBIiQ3YTAwMjY1Mi1mZjM5LTQ1NGUtODgxNC1kNjNjNTk1ZWI3Y",
                        "page_age": "April 30, 2025"
                    }
                ]
            },
            {
                "type": "text",
                "text": "Claude Shannon was born on April 30, 1916, in Petoskey, Michigan",
                "citations": [
                    {
                        "type": "web_search_result_location",
                        "url": "https://en.wikipedia.org/wiki/Claude_Shannon",
                        "title": "Claude Shannon - Wikipedia",
                        "cited_text": "Claude Elwood Shannon (April 30, 1916 - February 24, 2001)",
                        "encrypted_index": "Eo8BCioIAhgBIiQyYjQ0OWJmZi1lNm"
                    }
                ]
            }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 100, "output_tokens": 40 }
    });

    let parsed = provider.parse_response(mock_resp).expect("parse response");
    let parts = &parsed.messages[0].parts;
    assert_eq!(parts.len(), 3);

    match &parts[1] {
        ContentPart::WebSearchCall(call) => {
            assert_eq!(call.id, "srvtoolu_01WYG3ziw53XMcoyKL4XcZmE");
            assert_eq!(call.query, "claude shannon birth date");
            assert_eq!(call.status, SearchStatus::Completed);
            assert_eq!(call.sources.len(), 1);
            assert_eq!(
                call.sources[0].url,
                "https://en.wikipedia.org/wiki/Claude_Shannon"
            );
        }
        _ => panic!("Expected WebSearchCall"),
    }
    match &parts[2] {
        ContentPart::Text { text, annotations } => {
            assert!(text.contains("April 30, 1916"));
            assert_eq!(
                annotations,
                &vec![Annotation::WebSearchResultLocation {
                    url: "https://en.wikipedia.org/wiki/Claude_Shannon".into(),
                    title: "Claude Shannon - Wikipedia".into(),
                    cited_text: "Claude Elwood Shannon (April 30, 1916 - February 24, 2001)".into(),
                }]
            );
        }
        _ => panic!("Expected Text with citations"),
    }

    // 多轮重建：assistant 原生块链逐块原样回传（encrypted_content / encrypted_index 不变）
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("When was Shannon born?")));
    let mut asst_turn = Turn::assistant_turn(parsed.messages);
    asst_turn.meta = parsed.meta;
    session.append_turn(asst_turn);
    session.append_turn(Turn::user_turn(Message::text("Cite the source.")));

    let options = CompletionOptions {
        model: "claude-sonnet-4-5".into(),
        enable_web_search: true,
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");

    // 服务端工具声明：最新版本 + 直连调用（无 programmatic tool calling 能力）
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools[0]["type"], "web_search_20260318");
    assert_eq!(tools[0]["name"], "web_search");
    assert_eq!(tools[0]["allowed_callers"], serde_json::json!(["direct"]));
    assert_eq!(tools[1]["type"], "web_fetch_20260318");
    assert_eq!(tools[1]["name"], "web_fetch");
    assert_eq!(tools[1]["allowed_callers"], serde_json::json!(["direct"]));

    let asst_content = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(
        asst_content[2]["content"][0]["encrypted_content"],
        "EqgfCioIARgBIiQ3YTAwMjY1Mi1mZjM5LTQ1NGUtODgxNC1kNjNjNTk1ZWI3Y"
    );
    assert_eq!(
        asst_content[3]["citations"][0]["encrypted_index"],
        "Eo8BCioIAhgBIiQyYjQ0OWJmZi1lNm"
    );
}

/// annotation 关闭 web_fetch：仅声明 web_search，
/// 保住会被未接线 web_fetch 的中转端点整组丢弃的原生搜索
#[test]
fn test_anthropic_web_fetch_declaration_disabled_by_annotation() {
    let provider = AnthropicProvider::new("test-key");
    let session = Session::new(PathBuf::from("/tmp"), None);
    let options = CompletionOptions {
        model: "claude-haiku-4-5".into(),
        enable_web_search: true,
        enable_web_fetch: false,
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "web_search_20260318");
    assert_eq!(tools[0]["name"], "web_search");
}

#[test]
fn web_fetch_can_be_declared_without_web_search() {
    let provider = AnthropicProvider::new("test-key");
    let session = Session::new(PathBuf::from("/tmp"), None);
    let options = CompletionOptions {
        model: "claude-haiku-4-5".into(),
        enable_web_search: false,
        enable_web_fetch: true,
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .unwrap();
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "web_fetch");
    assert_eq!(tools[0]["type"], "web_fetch_20260318");
}

/// 官方 web fetch 响应样例：调用对投影为 OpenPage，document 内容块回传保真
#[test]
fn test_anthropic_web_fetch_parse_and_native_echo() {
    let provider = AnthropicProvider::new("test-key");
    let mock_resp = serde_json::json!({
        "id": "msg_wf_1",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "text", "text": "Let me fetch that article." },
            {
                "type": "server_tool_use",
                "id": "srvtoolu_fetch_1",
                "name": "web_fetch",
                "input": { "url": "https://example.com/article" }
            },
            {
                "type": "web_fetch_tool_result",
                "tool_use_id": "srvtoolu_fetch_1",
                "content": {
                    "type": "web_fetch_result",
                    "url": "https://example.com/article",
                    "content": {
                        "type": "document",
                        "source": {
                            "type": "text",
                            "media_type": "text/plain",
                            "data": "Full text content of the article..."
                        },
                        "title": "Article Title"
                    },
                    "retrieved_at": "2025-08-25T10:30:00Z"
                }
            },
            { "type": "text", "text": "The article says hi." }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 50, "output_tokens": 20 }
    });

    let parsed = provider
        .parse_response(mock_resp.clone())
        .expect("parse response");
    let parts = &parsed.messages[0].parts;
    assert_eq!(parts.len(), 3);
    match &parts[1] {
        ContentPart::WebSearchCall(call) => {
            assert_eq!(call.id, "srvtoolu_fetch_1");
            assert_eq!(
                call.action,
                Some(WebSearchAction::OpenPage {
                    url: "https://example.com/article".into()
                })
            );
            assert_eq!(call.status, SearchStatus::Completed);
            assert!(call.sources.is_empty());
        }
        _ => panic!("Expected OpenPage WebSearchCall"),
    }

    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("Fetch the article")));
    let mut asst_turn = Turn::assistant_turn(parsed.messages);
    asst_turn.meta = parsed.meta;
    session.append_turn(asst_turn);
    let body = provider
        .build_request_body(
            &session,
            &[],
            &CompletionOptions {
                model: "claude-sonnet-4-5".into(),
                enable_web_search: true,
                ..Default::default()
            },
        )
        .expect("build body");
    assert_eq!(
        body["messages"][1]["content"].as_array().unwrap(),
        mock_resp["content"].as_array().unwrap()
    );
}

/// 服务端工具执行失败仍为 HTTP 200：错误结果按 Failed 状态上报
#[test]
fn test_anthropic_search_error_result_marks_failed() {
    let provider = AnthropicProvider::new("test-key");
    let mock_resp = serde_json::json!({
        "id": "msg_err_1",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "server_tool_use",
                "id": "srvtoolu_err",
                "name": "web_search",
                "input": { "query": "blocked query" }
            },
            {
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_err",
                "content": {
                    "type": "web_search_tool_result_error",
                    "error_code": "max_uses_exceeded"
                }
            }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 30, "output_tokens": 10 }
    });
    let parsed = provider.parse_response(mock_resp).expect("parse response");
    match &parsed.messages[0].parts[0] {
        ContentPart::WebSearchCall(call) => {
            assert_eq!(call.query, "blocked query");
            assert_eq!(call.status, SearchStatus::Failed);
            assert!(call.sources.is_empty());
        }
        _ => panic!("Expected WebSearchCall"),
    }
}

/// 官方流式序列：server_tool_use 经 input_json_delta 增量下发，结果块整块到达，
/// 引用经 citations_delta 逐条追加；流式与非流式产出同一条原生块链
#[tokio::test]
async fn test_anthropic_web_search_streaming_matches_parse() {
    let provider = AnthropicProvider::new("test-key");
    let (tx, mut rx) = mpsc::channel(16);
    let mut handler = AnthropicStreamHandler {
        blocks: Vec::new(),
        partial_inputs: HashMap::new(),
        usage: AnthropicUsageSnapshot::default(),
        forwarder: EventForwarder::new(Some(tx)),
    };
    let events = [
        serde_json::json!({
            "type": "message_start",
            "message": { "usage": { "input_tokens": 60 } }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "text", "text": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "Searching." }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 1,
            "content_block": { "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search" }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 1,
            "delta": { "type": "input_json_delta", "partial_json": "{\"query\":" }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 1,
            "delta": { "type": "input_json_delta", "partial_json": "\"rust edition\"}" }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 2,
            "content_block": {
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_1",
                "content": [
                    {
                        "type": "web_search_result",
                        "url": "https://blog.rust-lang.org",
                        "title": "Rust Blog",
                        "encrypted_content": "Enc1"
                    }
                ]
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 3,
            "content_block": { "type": "text", "text": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 3,
            "delta": { "type": "text_delta", "text": "Rust 2024 is out." }
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 3,
            "delta": {
                "type": "citations_delta",
                "citation": {
                    "type": "web_search_result_location",
                    "url": "https://blog.rust-lang.org",
                    "title": "Rust Blog",
                    "cited_text": "Rust 2024 is out",
                    "encrypted_index": "Idx1"
                }
            }
        }),
        serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 30 }
        }),
        serde_json::json!({ "type": "message_stop" }),
    ];
    for ev in events {
        handler.on_event(ev).await.expect("handle stream event");
    }
    let (blocks, usage) = handler.finish().expect("finish");

    // 事件流按序转发：文本增量 → 搜索结果 → 文本增量 → 引用
    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        seen.push(event);
    }
    assert_eq!(seen.len(), 4);
    assert!(
        matches!(&seen[0], AgentEvent::Text { text_id, delta } if text_id == "0" && delta == "Searching.")
    );
    match &seen[1] {
        AgentEvent::WebSearchCall(call) => {
            assert_eq!(call.id, "srvtoolu_1");
            assert_eq!(call.query, "rust edition");
            assert_eq!(call.status, SearchStatus::Completed);
            assert_eq!(call.sources.len(), 1);
            assert_eq!(call.sources[0].url, "https://blog.rust-lang.org");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(
        matches!(&seen[2], AgentEvent::Text { text_id, delta } if text_id == "3" && delta == "Rust 2024 is out.")
    );
    match &seen[3] {
        AgentEvent::Citation {
            text_id,
            annotation,
        } => {
            assert_eq!(text_id, "3");
            assert_eq!(
                annotation,
                &Annotation::WebSearchResultLocation {
                    url: "https://blog.rust-lang.org".into(),
                    title: "Rust Blog".into(),
                    cited_text: "Rust 2024 is out".into(),
                }
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // 流式组装与非流式解析同源：领域投影与原生块链完全一致
    let streamed = assemble_response(blocks, usage).expect("assemble");
    let parsed = provider
        .parse_response(serde_json::json!({
            "id": "msg_stream_ws",
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "text", "text": "Searching." },
                {
                    "type": "server_tool_use",
                    "id": "srvtoolu_1",
                    "name": "web_search",
                    "input": { "query": "rust edition" }
                },
                {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_1",
                    "content": [
                        {
                            "type": "web_search_result",
                            "url": "https://blog.rust-lang.org",
                            "title": "Rust Blog",
                            "encrypted_content": "Enc1"
                        }
                    ]
                },
                {
                    "type": "text",
                    "text": "Rust 2024 is out.",
                    "citations": [
                        {
                            "type": "web_search_result_location",
                            "url": "https://blog.rust-lang.org",
                            "title": "Rust Blog",
                            "cited_text": "Rust 2024 is out",
                            "encrypted_index": "Idx1"
                        }
                    ]
                }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 60, "output_tokens": 30 }
        }))
        .expect("parse full");
    assert_eq!(streamed.messages[0].parts, parsed.messages[0].parts);
    assert_eq!(streamed.meta, parsed.meta);
}

#[test]
fn test_anthropic_scope_mismatch_rebuilds_and_projects_web_call() {
    let provider = AnthropicProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("搜一下 rust async")));

    // 前一亲和组（外置时代）的搜索调用：按当前工具面投影
    let call = WebSearchCall::completed(
        "ws_1",
        "rust async",
        vec![WebSearchSource::new("https://example.com", "Example", None)],
    );
    session.append_turn(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::WebSearchCall(call),
    ])]));

    let tools = vec![ToolDefinition {
        name: "web_search".into(),
        description: "d".into(),
        parameters: serde_json::json!({ "type": "object" }),
    }];

    // 仅外置客户端函数在位 → tool_use + 紧随 user 消息上的 tool_result
    let options = CompletionOptions {
        model: "claude-x".into(),
        enable_web_search: false,
        affinity: Some("homo/deepseek".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &tools, &options)
        .expect("build body");
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
    assert_eq!(msgs[1]["content"][0]["name"], "web_search");
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "ws_1");
    assert!(
        msgs[2]["content"][0]["content"]
            .as_str()
            .unwrap()
            .contains("example.com")
    );

    // 原生 server tool 在位 → server_tool_use + web_search_tool_result
    let options_native = CompletionOptions {
        model: "claude-x".into(),
        enable_web_search: true,
        affinity: Some("homo/claude".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options_native)
        .expect("build body");
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["content"][0]["type"], "server_tool_use");
    assert_eq!(msgs[1]["content"][0]["name"], "web_search");
    assert_eq!(msgs[1]["content"][0]["input"]["query"], "rust async");
    assert_eq!(msgs[1]["content"][1]["type"], "web_search_tool_result");
}

#[test]
fn test_anthropic_native_blocks_skipped_on_affinity_mismatch() {
    let provider = AnthropicProvider::new("test-key");
    let mock_resp = serde_json::json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "thinking", "thinking": "secret reasoning", "signature": "sig_a" },
            { "type": "text", "text": "answer" }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    });
    let parsed = provider.parse_response(mock_resp).expect("parse response");
    let mut asst_turn = Turn::assistant_turn(parsed.messages);
    asst_turn.meta = parsed.meta;
    asst_turn.meta.anthropic.as_mut().unwrap().scope = Some("homo/claude".into());
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(asst_turn);
    session.append_turn(Turn::user_turn(Message::text("继续")));

    // 切换亲和组：签名块绑定签发者不可回传，降维为通用重建（thinking 丢弃，text 保留）
    let options = CompletionOptions {
        model: "claude-x".into(),
        affinity: Some("other".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");
    let text = serde_json::to_string(&body).unwrap();
    assert!(!text.contains("sig_a"), "签名不可跨组回传: {text}");
    assert!(!text.contains("secret reasoning"), "{text}");
    assert!(text.contains("answer"), "{text}");
}
