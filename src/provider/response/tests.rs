use crate::event::AgentEvent;
use crate::model::{
    Annotation, ContentPart, Message, ResponseOutputItem, ResponseTurnMeta,
    ResponseWebSearchAction, Role, Session, ToolCall, WebSearchAction, WebSearchCall,
};
use crate::provider::http::SseEventHandler;
use crate::provider::{CompletionOptions, ProviderError};

use super::stream::*;

use super::*;
use crate::model::*;
use std::path::PathBuf;

#[test]
fn media_tool_results_use_native_content_blocks() {
    let session = super::super::common::media_tool_session();
    let body = ResponseProvider::new("test")
        .build_request_body(&session, &[], &CompletionOptions::default())
        .unwrap();
    let output = &body["input"][1]["output"];
    assert_eq!(body["input"][1]["call_id"], "read_media");
    assert_eq!(output[0]["type"], "input_text");
    assert_eq!(output[1]["type"], "input_image");
    assert!(
        output[1]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,iVBOR")
    );
}

#[test]
fn test_response_provider_payload_construction_and_previous_id() {
    let provider = ResponseProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);

    let user_msg = Message::text("Check the weather in Tokyo");
    let user_turn = Turn::user_turn(user_msg);
    session.append_turn(user_turn);

    // 第一轮：无 previous_response_id，携带 reasoning_effort
    let options = CompletionOptions {
        model: "gpt-4.5-preview".into(),
        enable_web_search: true,
        reasoning_effort: Some("medium".into()),
        ..Default::default()
    };
    let body1 = provider
        .build_request_body(&session, &[], &options)
        .expect("build body1");
    assert!(body1.get("previous_response_id").is_none());
    assert!(body1.get("max_output_tokens").is_none());
    assert_eq!(body1["tools"][0]["type"], "web_search");
    // Responses API 标准：reasoning.effort，禁止 top-level reasoning_effort
    assert_eq!(body1["reasoning"]["effort"], "medium");
    // 思考摘要需显式请求，否则 reasoning item 的 summary 恒为空
    assert_eq!(body1["reasoning"]["summary"], "concise");
    assert!(body1.get("reasoning_effort").is_none());

    // 模拟第一轮 Assistant 成功返回 response_id = "resp_turn_1"
    let mut asst_turn =
        Turn::assistant_turn(vec![Message::new(vec![ContentPart::text("It is sunny.")])]);
    asst_turn.meta.response = Some(ResponseTurnMeta::new("resp_turn_1"));
    session.append_turn(asst_turn);

    // 用户提出第二轮需求
    let user_msg2 = Message::text("What about tomorrow?");
    session.append_turn(Turn::user_turn(user_msg2));

    // 第二轮：应自动提取 previous_response_id: "resp_turn_1"
    let body2 = provider
        .build_request_body(&session, &[], &options)
        .expect("build body2");
    assert_eq!(body2["previous_response_id"], "resp_turn_1");
    // 增量 input 只包含第二轮的用户提问
    let inputs = body2["input"].as_array().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["content"][0]["text"], "What about tomorrow?");
    assert_eq!(body2["reasoning"]["effort"], "medium");
    assert_eq!(body2["reasoning"]["summary"], "concise");
}

#[test]
fn test_response_provider_previous_id_with_tool_call_output_pair() {
    let provider = ResponseProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);

    // 1. 用户提问
    session.append_turn(Turn::user_turn(Message::text("run dir")));

    // 2. Assistant 返回 ToolCall，并生成 response_id
    let mut asst_turn =
        Turn::assistant_turn(vec![Message::new(vec![ContentPart::ToolCall(ToolCall {
            id: "call_abc123".into(),
            name: "powershell".into(),
            arguments: r#"{"command":"dir"}"#.into(),
        })])]);
    asst_turn.meta.response = Some(ResponseTurnMeta::new("resp_asst_1"));
    session.append_turn(asst_turn);

    // 3. 执行工具并返回 ToolResult
    session.append_turn(Turn::new(
        Role::Tool,
        vec![Message::tool_result(ToolResult::success(
            "call_abc123",
            "dir output",
        ))],
    ));

    let options = CompletionOptions {
        model: "gpt-5.6-terra".into(),
        ..Default::default()
    };

    // 4. 构建下次请求 Payload：需携带 previous_response_id，且 input 必须同时包含 function_call 和 function_call_output
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");
    assert_eq!(body["previous_response_id"], "resp_asst_1");

    let inputs = body["input"].as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0]["type"], "function_call");
    assert_eq!(inputs[0]["call_id"], "call_abc123");
    assert_eq!(inputs[0]["name"], "powershell");

    assert_eq!(inputs[1]["type"], "function_call_output");
    assert_eq!(inputs[1]["call_id"], "call_abc123");
    assert_eq!(inputs[1]["output"], "dir output");
}

#[test]
fn test_response_provider_parse_with_search_and_citations() {
    let provider = ResponseProvider::new("test-key");
    let mock_json = serde_json::json!({
        "id": "resp_98765",
        "status": "completed",
        "store": true,
        "reasoning": {
            "effort": "medium"
        },
        "output": [
            {
                "type": "web_search_call",
                "id": "call_s1",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": ["tokyo weather"],
                    "sources": [
                        {
                            "type": "url",
                            "url": "https://weather.com/tokyo"
                        }
                    ]
                }
            },
            {
                "type": "message",
                "id": "msg_search_1",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Tokyo is sunny with 22C today [1].",
                        "annotations": [
                            {
                                "type": "url_citation",
                                "url": "https://weather.com/tokyo",
                                "title": "Tokyo Weather Forecast",
                                "start_index": 30,
                                "end_index": 33
                            }
                        ]
                    }
                ]
            }
        ],
        "usage": {
            "input_tokens": 150,
            "output_tokens": 40,
            "total_tokens": 190,
            "input_tokens_details": {
                "cached_tokens": 0,
                "cache_write_tokens": 0
            },
            "output_tokens_details": {
                "reasoning_tokens": 0
            }
        }
    });

    let parsed = provider.parse_response(mock_json).expect("parse response");
    let resp_meta = parsed.meta.response.as_ref().unwrap();
    assert_eq!(resp_meta.response_id.as_deref(), Some("resp_98765"));
    assert_eq!(resp_meta.reasoning_effort.as_deref(), Some("medium"));
    let msg = &parsed.messages[0];
    assert_eq!(msg.parts.len(), 2);

    // 验证 WebSearchCall 映射
    match &msg.parts[0] {
        ContentPart::WebSearchCall(call) => {
            assert_eq!(call.id, "call_s1");
            assert_eq!(call.query, "tokyo weather");
            assert_eq!(call.sources.len(), 1);
            assert_eq!(call.sources[0].url, "https://weather.com/tokyo");
        }
        _ => panic!("Expected WebSearchCall"),
    }

    // 验证带 Annotation 的 Text 映射
    match &msg.parts[1] {
        ContentPart::Text { text, annotations } => {
            assert!(text.contains("Tokyo is sunny"));
            assert_eq!(annotations.len(), 1);
            if let Annotation::UrlCitation {
                start_index,
                end_index,
                ..
            } = annotations[0]
            {
                assert_eq!(start_index, 30);
                assert_eq!(end_index, 33);
            } else {
                panic!("Expected UrlCitation");
            }
        }
        _ => panic!("Expected Text with annotations"),
    }
}

#[test]
fn test_response_provider_parse_preserves_output_order() {
    let provider = ResponseProvider::new("test-key");
    let mock_json = serde_json::json!({
        "id": "resp_grok_1",
        "status": "completed",
        "output": [
            {
                "type": "reasoning",
                "id": "rs_1",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "Analyzing user query."}]
            },
            {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "Here is the summary."}]
            },
            {
                "type": "web_search_call",
                "id": "call_s1",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": ["Claude Code updates"],
                    "sources": [{"type": "url", "url": "https://code.claude.com"}]
                }
            }
        ]
    });

    let parsed = provider.parse_response(mock_json).expect("parse response");
    let parts = &parsed.messages[0].parts;
    assert_eq!(parts.len(), 3);
    assert!(matches!(parts[0], ContentPart::Reasoning { .. }));
    assert!(matches!(parts[1], ContentPart::Text { .. }));
    assert!(matches!(parts[2], ContentPart::WebSearchCall(_)));

    // 强类型原生 output_items 顺序保持与厂商完全一致
    let meta_items = &parsed.meta.response.as_ref().unwrap().output_items;
    assert_eq!(meta_items.len(), 3);
    assert!(matches!(
        meta_items[0],
        ResponseOutputItem::Reasoning { .. }
    ));
    assert!(matches!(meta_items[1], ResponseOutputItem::Message { .. }));
    assert!(matches!(
        meta_items[2],
        ResponseOutputItem::WebSearchCall { .. }
    ));
}

#[test]
fn test_response_provider_cold_start_history_and_temperature_guard() {
    let provider =
        ResponseProvider::new("test-key").with_base_url("https://api.openai.com/v1/responses/");
    let mut session = Session::new(PathBuf::from("/tmp"), None);

    // System 提示
    session.append_turn(Turn::new(Role::System, vec![Message::text("Be concise.")]));
    // User 问题
    session.append_turn(Turn::user_turn(Message::text("Hello")));
    // Assistant 回复且包含 ToolCall（来自其他 provider 或无 previous_response_id）
    let asst_msg = Message::new(vec![
        ContentPart::text("Calling tool"),
        ContentPart::ToolCall(ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: r#"{"command":"pwd"}"#.into(),
        }),
    ]);
    session.append_turn(Turn::assistant_turn(vec![asst_msg]));
    // Tool 返回
    session.append_turn(Turn::new(
        Role::Tool,
        vec![Message::tool_result(ToolResult::success(
            "c1",
            "/home/project",
        ))],
    ));

    let options = CompletionOptions {
        model: "gpt-5.6-luna".into(),
        temperature: Some(0.7),
        reasoning_effort: Some("high".into()),
        enable_web_search: true,
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");

    // 验证推理模式下 temperature 被安全抑制
    assert!(body.get("temperature").is_none());
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["reasoning"]["summary"], "concise");

    // 验证冷启动时系统消息与 Assistant 消息均被序列化进 input
    let input = body["input"].as_array().unwrap();
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "system");
    assert_eq!(input[0]["content"][0]["text"], "Be concise.");

    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[1]["role"], "user");

    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["name"], "bash");

    assert_eq!(input[3]["type"], "message");
    assert_eq!(input[3]["role"], "assistant");
    assert_eq!(input[3]["content"][0]["type"], "output_text");

    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["call_id"], "c1");
}

#[test]
fn test_response_provider_service_tier_injection() {
    let provider = ResponseProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("Hello")));

    // annotation.fast: true 时请求体携带 service_tier
    let tiered = CompletionOptions {
        model: "gpt-5.6-luna".into(),
        service_tier: Some("fast".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &tiered)
        .expect("build body");
    assert_eq!(body["service_tier"], "fast");

    // 缺省时不携带该字段
    let plain = CompletionOptions {
        model: "gpt-5.6-luna".into(),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &plain)
        .expect("build body");
    assert!(body.get("service_tier").is_none());
}

#[test]
fn test_response_provider_parse_standard_output_text_and_reasoning() {
    let provider = ResponseProvider::new("test-key");

    // 标准 OpenAI Responses API: output[].content[].type == "output_text"，以及 reasoning 块与 item_id 绑定
    let json_standard = serde_json::json!({
        "id": "resp_std_001",
        "status": "completed",
        "store": true,
        "output": [
            {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [
                    {
                        "type": "summary_text",
                        "text": "Analyzing the user query step by step"
                    }
                ]
            },
            {
                "type": "message",
                "id": "msg_std_100",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Here is the exact response from the model.",
                        "annotations": []
                    }
                ]
            },
            {
                "type": "function_call",
                "id": "fc_fn_1",
                "call_id": "call_fn_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hello\"}",
                "status": "completed"
            }
        ],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20,
            "total_tokens": 30,
            "input_tokens_details": {
                "cached_tokens": 0,
                "cache_write_tokens": 0
            },
            "output_tokens_details": {
                "reasoning_tokens": 0
            }
        }
    });

    let parsed = provider
        .parse_response(json_standard)
        .expect("parse standard response");
    assert_eq!(parsed.messages.len(), 1);
    let msg = &parsed.messages[0];
    assert!(
        parsed
            .meta
            .response
            .as_ref()
            .unwrap()
            .output_items
            .iter()
            .any(|item| {
                matches!(item, ResponseOutputItem::Message { id, .. } if id == "msg_std_100")
            })
    );
    let parts = &msg.parts;
    assert_eq!(parts.len(), 3);

    match &parts[0] {
        ContentPart::Reasoning { content, .. } => {
            assert_eq!(content, "Analyzing the user query step by step");
        }
        _ => panic!("Expected Reasoning part"),
    }

    match &parts[1] {
        ContentPart::Text { text, .. } => {
            assert_eq!(text, "Here is the exact response from the model.");
        }
        _ => panic!("Expected Text part"),
    }

    match &parts[2] {
        ContentPart::ToolCall(call) => {
            assert_eq!(call.id, "call_fn_1");
            assert_eq!(call.name, "bash");
            assert_eq!(call.arguments, "{\"command\":\"echo hello\"}");
        }
        _ => panic!("Expected ToolCall part"),
    }
}

#[test]
fn test_response_provider_store_false_falls_back_to_full_history() {
    let provider = ResponseProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);

    // 1. 用户第一轮提问
    let user_msg1 = Message::text("第一轮示例问题");
    session.append_turn(Turn::user_turn(user_msg1));

    // 2. 模拟服务端返回 store: false 的响应报文
    let mock_unstored_json = serde_json::json!({
        "id": "resp_unstored_123",
        "status": "completed",
        "store": false,
        "previous_response_id": null,
        "output": [
            {
                "type": "reasoning",
                "id": "rs_ephemeral_1",
                "summary": [],
                "encrypted_content": "encrypted-reasoning"
            },
            {
                "type": "web_search_call",
                "id": "ws_ephemeral_1",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": ["example query"],
                    "sources": []
                }
            },
            {
                "type": "message",
                "id": "msg_ephemeral_1",
                "role": "assistant",
                "status": "completed",
                "phase": "final_answer",
                "content": [
                    {
                        "type": "output_text",
                        "text": "第一轮示例答复。",
                        "annotations": []
                    }
                ]
            }
        ],
        "usage": {
            "input_tokens": 100,
            "output_tokens": 20,
            "total_tokens": 120,
            "input_tokens_details": {
                "cached_tokens": 0,
                "cache_write_tokens": 0
            },
            "output_tokens_details": {
                "reasoning_tokens": 0
            }
        }
    });

    let parsed = provider
        .parse_response(mock_unstored_json)
        .expect("parse unstored response");
    let resp_meta = parsed.meta.response.as_ref().unwrap();
    // 校验：当 store: false 时不记录 response_id、previous_response_id，且 store 为 false
    assert!(resp_meta.response_id.is_none());
    assert!(resp_meta.previous_response_id.is_none());
    assert!(!resp_meta.store);
    // 不记录依赖 store 的 message item_id

    let mut asst_turn = Turn::assistant_turn(parsed.messages);
    asst_turn.meta = parsed.meta;
    assert!(asst_turn.openai_response_id().is_none());
    session.append_turn(asst_turn);

    // 会话无法提取有效 response_id
    assert!(session.latest_openai_response_id(None).is_none());

    // 3. 用户发起第二轮提问
    let user_msg2 = Message::text("第二轮示例问题");
    session.append_turn(Turn::user_turn(user_msg2));

    let options = CompletionOptions {
        model: "gpt-5.6-terra".into(),
        enable_web_search: true,
        reasoning_effort: Some("medium".into()),
        ..Default::default()
    };

    // 4. 构建第二轮请求体：因未留存，必须回退至全量发送 input，且不传 previous_response_id
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");
    assert!(body.get("previous_response_id").is_none());
    assert!(body.get("store").is_none());
    assert_eq!(body["prompt_cache_key"], session.header.id);
    assert_eq!(
        body["include"],
        serde_json::json!([
            "reasoning.encrypted_content",
            "web_search_call.action.sources"
        ])
    );

    let input_items = body["input"].as_array().unwrap();
    // 原样回传第一轮 user、完整原生 output items 以及第二轮 user
    assert_eq!(input_items.len(), 5);
    assert_eq!(input_items[0]["role"], "user");
    assert_eq!(input_items[0]["content"][0]["text"], "第一轮示例问题");
    assert_eq!(input_items[1]["type"], "reasoning");
    assert_eq!(input_items[1]["encrypted_content"], "encrypted-reasoning");
    // 回传 input 的 reasoning item 必须携带 summary 字段（即使为空数组）
    assert_eq!(input_items[1]["summary"], serde_json::json!([]));
    assert_eq!(input_items[2]["type"], "web_search_call");
    assert_eq!(
        input_items[2]["action"]["queries"],
        serde_json::json!(["example query"])
    );
    assert_eq!(input_items[3]["role"], "assistant");
    assert_eq!(input_items[3]["phase"], "final_answer");
    assert_eq!(input_items[3]["content"][0]["text"], "第一轮示例答复。");
    assert_eq!(input_items[4]["role"], "user");
    assert_eq!(input_items[4]["content"][0]["text"], "第二轮示例问题");
}

#[test]
fn test_response_provider_missing_store_is_not_stored() {
    let provider = ResponseProvider::new("test-key");
    let mock_json_no_store = serde_json::json!({
        "id": "resp_test_1",
        "status": "completed",
        "output": []
    });
    let parsed = provider
        .parse_response(mock_json_no_store)
        .expect("parse response without store");
    let meta = parsed.meta.response.as_ref().unwrap();
    assert!(meta.response_id.is_none());
    assert!(!meta.store);
}

#[test]
fn test_response_provider_failed_status_returns_protocol_error() {
    let provider = ResponseProvider::new("test-key");
    let mock_failed_json = serde_json::json!({
        "id": "resp_fail_1",
        "status": "failed",
        "store": false,
        "error": {
            "code": "server_error",
            "message": "The model failed to generate a response."
        },
        "output": []
    });
    let result = provider.parse_response(mock_failed_json);
    assert!(
        matches!(result, Err(ProviderError::Protocol(msg)) if msg.contains("Response failed: The model failed to generate a response."))
    );
}

#[tokio::test]
async fn test_stream_handler_separates_summary_parts_with_newline() {
    let provider = ResponseProvider::new("test-key");
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
    let mut handler = ResponseStreamHandler::new(&provider, Some(tx));

    // 官方规范流序列：首段 part.added(summary_index=0) → delta → 次段 part.added(1) → delta
    for ev in [
        serde_json::json!({"type": "response.reasoning_summary_part.added", "summary_index": 0}),
        serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "part one"}),
        serde_json::json!({"type": "response.reasoning_summary_part.added", "summary_index": 1}),
        serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "part two"}),
    ] {
        handler.on_event(ev).await.unwrap();
    }

    let mut acc = String::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::Thought(text) = event {
            acc.push_str(&text);
        }
    }
    assert_eq!(acc, "part one\n\npart two");

    // summary_index 为规范必填字段，缺失即报协议错误
    let result = handler
        .on_event(serde_json::json!({"type": "response.reasoning_summary_part.added"}))
        .await;
    assert!(
        matches!(result, Err(ProviderError::Protocol(msg)) if msg.contains("Missing 'summary_index'"))
    );
}

#[tokio::test]
async fn test_stream_search_and_citations_arrive_before_completion() {
    let provider = ResponseProvider::new("test-key");
    let (tx, mut rx) = mpsc::channel(16);
    let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
    let search = serde_json::json!({
        "id": "ws_1", "type": "web_search_call", "status": "completed",
        "action": {"type": "search", "queries": ["Rust release"],
            "sources": [{"type": "url", "url": "https://www.rust-lang.org/"}]}
    });
    handler
        .on_event(serde_json::json!({
            "type": "response.output_item.done", "output_index": 0,
            "sequence_number": 1, "item": search
        }))
        .await
        .unwrap();
    let call = match rx.try_recv().unwrap() {
        AgentEvent::WebSearchCall(call) => call,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(call.id, "ws_1");
    assert_eq!(call.sources.len(), 1);

    let annotation = serde_json::json!({
        "type": "url_citation", "title": "Rust", "url": "https://www.rust-lang.org/",
        "start_index": 0, "end_index": 4
    });
    handler
        .on_event(serde_json::json!({
            "type": "response.output_text.delta", "item_id": "msg_1",
            "output_index": 1, "content_index": 0, "sequence_number": 2,
            "delta": "Rust"
        }))
        .await
        .unwrap();
    handler
        .on_event(serde_json::json!({
            "type": "response.output_text.annotation.added", "item_id": "msg_1",
            "output_index": 1, "content_index": 0, "annotation_index": 0,
            "sequence_number": 3, "annotation": annotation
        }))
        .await
        .unwrap();
    assert!(
        matches!(rx.try_recv().unwrap(), AgentEvent::Text { text_id, delta }
        if text_id == "msg_1:0" && delta == "Rust")
    );
    let citation: Annotation = serde_json::from_value(annotation.clone()).unwrap();
    assert!(
        matches!(rx.try_recv().unwrap(), AgentEvent::Citation { text_id, annotation }
        if text_id == "msg_1:0" && annotation == citation)
    );
    assert!(handler.completed.is_none());

    handler.on_event(serde_json::json!({
        "type": "response.completed", "sequence_number": 4,
        "response": {"id": "resp_1", "status": "completed", "output": [
            search,
            {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "Rust", "annotations": [annotation]}]}
        ]}
    })).await.unwrap();
    let response = handler.completed.as_ref().unwrap();
    assert_eq!(
        response.messages[0].parts[0],
        ContentPart::WebSearchCall(call)
    );
    assert_eq!(
        response.messages[0].parts[1],
        ContentPart::Text {
            text: "Rust".into(),
            annotations: vec![citation],
        }
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn test_stream_rejects_invalid_url_citation() {
    let provider = ResponseProvider::new("test-key");
    let mut handler = ResponseStreamHandler::new(&provider, None);
    let result = handler.on_event(serde_json::json!({
        "type": "response.output_text.annotation.added", "item_id": "msg_1",
        "output_index": 0, "content_index": 0, "annotation_index": 0,
        "sequence_number": 1,
        "annotation": {"type": "url_citation", "title": "Rust", "url": "https://www.rust-lang.org/"}
    })).await;
    assert!(
        matches!(result, Err(ProviderError::Protocol(message)) if message.contains("start_index"))
    );
}

#[tokio::test]
async fn test_stream_handler_reports_web_search_call_on_item_done() {
    let provider = ResponseProvider::new("test-key");
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
    let mut handler = ResponseStreamHandler::new(&provider, Some(tx));

    // 每个搜索条目完成时独立上报，保留动作类型。
    handler
        .on_event(serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "queries": ["example topic"]}
            }
        }))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AgentEvent::WebSearchCall(call)) => {
            assert_eq!(call.id, "ws_1");
            assert_eq!(call.query, "example topic");
            assert_eq!(call.action, Some(WebSearchAction::Search));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // 打开页面保留目标 URL，message 等其余项不产生搜索事件。
    handler
        .on_event(serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "web_search_call",
                "id": "ws_2",
                "status": "completed",
                "action": {"type": "open_page", "url": "https://example.com/article"}
            }
        }))
        .await
        .unwrap();
    handler
        .on_event(serde_json::json!({
            "type": "response.output_item.done",
            "item": {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "Answer", "annotations": []}]}
        }))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AgentEvent::WebSearchCall(call)) => {
            assert_eq!(
                call.action,
                Some(WebSearchAction::OpenPage {
                    url: "https://example.com/article".into(),
                })
            );
            assert!(call.query.is_empty());
            assert!(call.sources.is_empty());
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn test_web_actions_match_stream_final_and_storage() {
    let provider = ResponseProvider::new("test-key");
    let (tx, mut rx) = mpsc::channel(16);
    let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
    let actions = [
        serde_json::json!({"type": "search", "queries": ["query A", "query B"], "sources": [
            {"type": "url", "url": "https://example.com/a"},
            {"type": "url", "url": "https://example.com/b"}
        ]}),
        serde_json::json!({"type": "open_page", "url": "https://example.com/a"}),
        serde_json::json!({"type": "find_in_page", "url": "https://example.com/a", "pattern": "battery"}),
    ];
    let mut items = Vec::new();
    let mut streamed = Vec::new();
    for (index, action) in actions.into_iter().enumerate() {
        let item = serde_json::json!({
            "type": "web_search_call", "id": format!("ws_{index}"), "status": "completed", "action": action
        });
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done", "output_index": index,
                "sequence_number": index, "item": item
            }))
            .await
            .unwrap();
        let AgentEvent::WebSearchCall(call) = rx.try_recv().unwrap() else {
            panic!("expected search event");
        };
        assert_eq!(call.id, format!("ws_{index}"));
        let encoded = serde_json::to_value(&call).unwrap();
        assert_eq!(
            serde_json::from_value::<WebSearchCall>(encoded).unwrap(),
            call
        );
        streamed.push(ContentPart::WebSearchCall(call));
        items.push(item);
    }
    let ContentPart::WebSearchCall(search) = &streamed[0] else {
        unreachable!()
    };
    assert_eq!(search.action, Some(WebSearchAction::Search));
    assert_eq!(search.query, "query A, query B");
    assert_eq!(search.sources.len(), 2);
    let ContentPart::WebSearchCall(open) = &streamed[1] else {
        unreachable!()
    };
    assert_eq!(
        open.action,
        Some(WebSearchAction::OpenPage {
            url: "https://example.com/a".into()
        })
    );
    assert!(open.sources.is_empty());
    let ContentPart::WebSearchCall(find) = &streamed[2] else {
        unreachable!()
    };
    assert_eq!(
        find.action,
        Some(WebSearchAction::FindInPage {
            url: "https://example.com/a".into(),
            pattern: "battery".into(),
        })
    );
    assert!(find.sources.is_empty());
    let final_response = provider
        .parse_response(serde_json::json!({
            "id": "resp_1", "status": "completed", "output": items,
        }))
        .unwrap();
    assert_eq!(final_response.messages[0].parts, streamed);
}

#[tokio::test]
async fn test_open_page_url_optional_per_spec() {
    let provider = ResponseProvider::new("test-key");
    for action in [
        serde_json::json!({"type": "open_page"}),
        serde_json::json!({"type": "open_page", "url": null}),
    ] {
        let item = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_open",
            "status": "completed",
            "action": action,
        });
        let encoded = serde_json::to_value(
            serde_json::from_value::<ResponseOutputItem>(item.clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(encoded["action"], serde_json::json!({"type": "open_page"}));

        let parsed = provider
            .parse_response(serde_json::json!({
                "id": "resp_1", "status": "completed", "output": [item.clone()],
            }))
            .unwrap();
        let ContentPart::WebSearchCall(call) = &parsed.messages[0].parts[0] else {
            panic!("expected open_page");
        };
        assert_eq!(
            call.action,
            Some(WebSearchAction::OpenPage { url: String::new() })
        );

        let (tx, mut rx) = mpsc::channel(4);
        let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done",
                "item": item
            }))
            .await
            .unwrap();
        let AgentEvent::WebSearchCall(streamed) = rx.try_recv().unwrap() else {
            panic!("expected search event");
        };
        assert_eq!(streamed.action, call.action);
    }
}

#[tokio::test]
async fn test_missing_web_search_action_projects_actionless_call() {
    let provider = ResponseProvider::new("test-key");
    let item = serde_json::json!({
        "type": "web_search_call",
        "id": "ws_missing",
        "status": "completed"
    });

    let parsed = provider
        .parse_response(serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [item.clone()]
        }))
        .unwrap();
    let ContentPart::WebSearchCall(call) = &parsed.messages[0].parts[0] else {
        panic!("expected web search call");
    };
    assert_eq!(call.action, None);
    assert!(call.query.is_empty());
    assert!(call.sources.is_empty());

    // 流式 done 事件同样如实投影缺席，且不再发 Notice
    let (tx, mut rx) = mpsc::channel(4);
    let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
    handler
        .on_event(serde_json::json!({
            "type": "response.output_item.done",
            "item": item
        }))
        .await
        .unwrap();
    assert!(matches!(
        rx.try_recv(),
        Ok(AgentEvent::WebSearchCall(WebSearchCall {
            action: None,
            ..
        }))
    ));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn test_stream_item_done_sources_survive_completed_aggregation() {
    let provider = ResponseProvider::new("test-key");
    let (tx, mut rx) = mpsc::channel(16);
    let mut handler = ResponseStreamHandler::new(&provider, Some(tx));
    let done_items = [
        serde_json::json!({
            "type": "web_search_call", "id": "ws_0", "status": "completed",
            "action": {"type": "search", "queries": ["query A"], "sources": [
                {"type": "url", "url": "https://a.example/1"}
            ]}
        }),
        serde_json::json!({
            "type": "web_search_call", "id": "ws_1", "status": "completed",
            "action": {"type": "search", "queries": ["query B"], "sources": [
                {"type": "url", "url": "https://b.example/2"}
            ]}
        }),
        serde_json::json!({
            "type": "web_search_call", "id": "ws_open", "status": "completed",
            "action": {"type": "open_page", "url": "https://a.example/1"}
        }),
    ];
    for (index, item) in done_items.iter().enumerate() {
        handler
            .on_event(serde_json::json!({
                "type": "response.output_item.done",
                "output_index": index,
                "sequence_number": index,
                "item": item
            }))
            .await
            .unwrap();
        assert!(matches!(
            rx.try_recv().unwrap(),
            AgentEvent::WebSearchCall(_)
        ));
    }

    let union = [
        serde_json::json!({"type": "url", "url": "https://a.example/1"}),
        serde_json::json!({"type": "url", "url": "https://b.example/2"}),
        serde_json::json!({"type": "url", "url": "https://c.example/3"}),
    ];
    handler
        .on_event(serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "status": "completed",
                "output": [
                    {
                        "type": "web_search_call", "id": "ws_0", "status": "completed",
                        "action": {"type": "search", "queries": ["query A"], "sources": union}
                    },
                    {
                        "type": "web_search_call", "id": "ws_1", "status": "completed",
                        "action": {"type": "search", "queries": ["query B"], "sources": union}
                    },
                    {
                        "type": "web_search_call", "id": "ws_open", "status": "completed",
                        "action": {"type": "open_page", "url": "https://a.example/1"}
                    }
                ]
            }
        }))
        .await
        .unwrap();

    let parsed = handler.completed.unwrap();
    let parts = &parsed.messages[0].parts;
    let ContentPart::WebSearchCall(first) = &parts[0] else {
        panic!("expected search");
    };
    assert_eq!(
        first
            .sources
            .iter()
            .map(|source| source.url.as_str())
            .collect::<Vec<_>>(),
        ["https://a.example/1"]
    );
    let ContentPart::WebSearchCall(second) = &parts[1] else {
        panic!("expected search");
    };
    assert_eq!(
        second
            .sources
            .iter()
            .map(|source| source.url.as_str())
            .collect::<Vec<_>>(),
        ["https://b.example/2"]
    );
    let ContentPart::WebSearchCall(open) = &parts[2] else {
        panic!("expected open_page");
    };
    assert_eq!(
        open.action,
        Some(WebSearchAction::OpenPage {
            url: "https://a.example/1".into()
        })
    );
    assert!(open.sources.is_empty());

    let ResponseOutputItem::WebSearchCall {
        action: Some(ResponseWebSearchAction::Search { sources, .. }),
        ..
    } = &parsed.meta.response.as_ref().unwrap().output_items[0]
    else {
        panic!("expected native search item");
    };
    assert_eq!(sources.len(), 3);
}

#[test]
fn test_response_provider_scope_mismatch_rebuilds_history() {
    let provider = ResponseProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("第一问")));

    let mut asst_turn = Turn::assistant_turn(vec![Message::new(vec![ContentPart::text("答一")])]);
    let mut meta = ResponseTurnMeta::new("resp_luna");
    meta.scope = Some("homo/gpt".into());
    asst_turn.meta = TurnMeta {
        response: Some(meta),
        ..Default::default()
    };
    session.append_turn(asst_turn);
    session.append_turn(Turn::user_turn(Message::text("第二问")));

    // 跨亲和组：锚点不可复用，回退全量重建（无 previous_response_id）
    let options = CompletionOptions {
        model: "deepseek".into(),
        affinity: Some("homo/deepseek".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");
    assert!(body.get("previous_response_id").is_none());
    assert_eq!(body["input"].as_array().unwrap().len(), 3);

    // 同亲和组不同模型（gpt 同系切换）：链式锚点保留
    let options_same = CompletionOptions {
        model: "terra".into(),
        affinity: Some("homo/gpt".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options_same)
        .expect("build body");
    assert_eq!(body["previous_response_id"], "resp_luna");
}

#[test]
fn test_response_provider_web_search_call_replays_per_tool_surface() {
    let provider = ResponseProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("search")));

    // 跨亲和组重建的搜索调用（纯数据，按当前工具面投影）
    let call = WebSearchCall::completed(
        "ws_1",
        "rust async",
        vec![crate::model::WebSearchSource::new(
            "https://example.com",
            "Example",
            None,
        )],
    );
    session.append_turn(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::WebSearchCall(call),
    ])]));

    let tools = vec![crate::model::ToolDefinition {
        name: "web_search".into(),
        description: "d".into(),
        parameters: serde_json::json!({ "type": "object" }),
    }];

    // 仅外置客户端函数在位 → 合成 function 调用与结果对
    let options = CompletionOptions {
        model: "m".into(),
        enable_web_search: false,
        affinity: Some("x".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &tools, &options)
        .expect("build body");
    let inputs = body["input"].as_array().unwrap();
    let call_item = inputs
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("function_call");
    assert_eq!(call_item["name"], "web_search");
    assert_eq!(call_item["call_id"], "ws_1");
    let output = inputs
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("function_call_output");
    assert!(output["output"].as_str().unwrap().contains("example.com"));

    // 原生 server tool 在位 → 原生 web_search_call 条目
    let options_native = CompletionOptions {
        model: "m".into(),
        enable_web_search: true,
        affinity: Some("x".into()),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &tools, &options_native)
        .expect("build body");
    let inputs = body["input"].as_array().unwrap();
    let item = inputs
        .iter()
        .find(|item| item["type"] == "web_search_call")
        .expect("web_search_call");
    assert_eq!(item["action"]["query"], "rust async");

    // open_page 随原生 web_search 回放，不降为客户端 web_fetch
    let mut open = WebSearchCall::completed("ws_open", "", Vec::new());
    open.action = Some(WebSearchAction::OpenPage {
        url: "https://example.com/article".into(),
    });
    session.append_turn(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::WebSearchCall(open),
    ])]));
    let body = provider
        .build_request_body(&session, &[], &options_native)
        .expect("build body");
    let open_item = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "ws_open")
        .expect("open_page web_search_call");
    assert_eq!(open_item["type"], "web_search_call");
    assert_eq!(open_item["action"]["type"], "open_page");
    assert_eq!(open_item["action"]["url"], "https://example.com/article");
}

#[test]
fn test_response_provider_parse_with_image_generation_call() {
    let provider = ResponseProvider::new("test-key");
    // 按官方规范构造：image_generation tool 产出的 server tool 条目
    let ig_item = serde_json::json!({
        "type": "image_generation_call",
        "id": "ig_123",
        "status": "completed",
        "revised_prompt": "A gray tabby cat hugging an otter with an orange scarf",
        "result": "iVBORw0KGgoAAAANSUhEUg==",
        "action": "generate",
        "background": "auto",
        "output_format": "png",
        "quality": "high",
        "size": "1024x1024"
    });
    let mock_json = serde_json::json!({
        "id": "resp_ig_1",
        "status": "completed",
        "store": true,
        "output": [
            ig_item,
            {
                "type": "message",
                "id": "msg_ig_1",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Here is your image."}]
            }
        ],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20,
            "total_tokens": 30,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0}
        }
    });

    let parsed = provider.parse_response(mock_json).unwrap();

    // 原生条目强类型保真：逐字段反序列化成功，序列化与官方报文一致（无损回放前提）
    let resp_meta = parsed.meta.response.as_ref().unwrap();
    let native = &resp_meta.output_items[0];
    let ResponseOutputItem::ImageGenerationCall {
        id,
        status,
        result,
        revised_prompt,
        size,
        ..
    } = native
    else {
        panic!("expected image_generation_call native item");
    };
    assert_eq!(id, "ig_123");
    assert_eq!(status, "completed");
    assert_eq!(result.as_deref(), Some("iVBORw0KGgoAAAANSUhEUg=="));
    assert_eq!(
        revised_prompt.as_deref(),
        Some("A gray tabby cat hugging an otter with an orange scarf")
    );
    assert_eq!(size.as_deref(), Some("1024x1024"));
    let encoded = serde_json::to_value(native).unwrap();
    assert_eq!(encoded, ig_item);

    // 领域投影：ImageGenerationCall part，result 按官方 output_format 映射 mime（缺省 png）
    let call_part = parsed
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .find_map(|p| match p {
            ContentPart::ImageGenerationCall(call) => Some(call),
            _ => None,
        })
        .expect("projected image generation call");
    assert_eq!(call_part.id, "ig_123");
    assert_eq!(call_part.status, ImageGenerationStatus::Completed);
    assert_eq!(
        call_part.revised_prompt.as_deref(),
        Some("A gray tabby cat hugging an otter with an orange scarf")
    );
    let MediaSource::Base64 { data, mime_type } = call_part.result.as_ref().unwrap() else {
        panic!("expected base64 result");
    };
    assert_eq!(data, "iVBORw0KGgoAAAANSUhEUg==");
    assert_eq!(mime_type, "image/png");

    // 回放：store:false 无链式锚点时全量重建，原生 image_generation_call 条目原样回传 input
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    let mut replay_meta = resp_meta.clone();
    replay_meta.store = false;
    replay_meta.response_id = None;
    replay_meta.previous_response_id = None;
    let mut asst_turn = Turn::assistant_turn(parsed.messages.clone());
    asst_turn.meta.response = Some(replay_meta);
    session.append_turn(Turn::user_turn(Message::text("draw a cat")));
    session.append_turn(asst_turn);
    session.append_turn(Turn::user_turn(Message::text("now make it realistic")));

    let body = provider
        .build_request_body(&session, &[], &CompletionOptions::default())
        .expect("build body");
    let inputs = body["input"].as_array().unwrap();
    let replayed = inputs
        .iter()
        .find(|item| item["type"] == "image_generation_call")
        .expect("replayed image_generation_call");
    assert_eq!(replayed, &ig_item);
}

#[test]
fn test_response_provider_parse_with_in_progress_image_generation_call() {
    let provider = ResponseProvider::new("test-key");
    // 按官方规范构造：生成中条目无 result
    let mock_json = serde_json::json!({
        "id": "resp_ig_2",
        "status": "in_progress",
        "store": true,
        "output": [
            {
                "type": "image_generation_call",
                "id": "ig_456",
                "status": "in_progress",
                "revised_prompt": "A gray tabby cat",
                "output_format": "webp",
                "quality": "high",
                "size": "1024x1024"
            }
        ],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20,
            "total_tokens": 30,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0}
        }
    });

    let parsed = provider.parse_response(mock_json).unwrap();

    // 未完成调用同样投影：状态行可见，无 result
    let call_part = parsed
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .find_map(|p| match p {
            ContentPart::ImageGenerationCall(call) => Some(call),
            _ => None,
        })
        .expect("projected in-progress call");
    assert_eq!(call_part.id, "ig_456");
    assert_eq!(call_part.status, ImageGenerationStatus::InProgress);
    assert_eq!(call_part.result, None);
}

#[test]
fn test_file_ref_projects_as_input_text() {
    let provider = ResponseProvider::new("test-key");
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::new(vec![
        ContentPart::text("解释 @src/main.rs"),
        ContentPart::FileRef {
            path: "src/main.rs".into(),
            content: "fn main() {}".into(),
            truncated: true,
        },
    ])));

    let body = provider
        .build_request_body(&session, &[], &CompletionOptions::default())
        .expect("build body");
    let content = body["input"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "input_text");
    assert_eq!(content[0]["text"], "解释 @src/main.rs");
    assert_eq!(content[1]["type"], "input_text");
    let text = content[1]["text"].as_str().unwrap();
    assert!(text.starts_with("@src/main.rs\nfn main() {}"));
    assert!(text.ends_with("[content truncated]"));
}
