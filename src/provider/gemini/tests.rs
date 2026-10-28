use super::*;
use crate::model::*;
use std::path::PathBuf;

#[test]
fn media_tool_results_use_native_content_blocks() {
    let session = crate::provider::common::media_tool_session();
    let body = GeminiProvider::new("test")
        .build_request_body(&session, &[], &CompletionOptions::default())
        .unwrap();
    let output = &body["contents"][1]["parts"][0]["functionResponse"];
    assert_eq!(output["name"], "Read");
    assert_eq!(
        body["contents"][1]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert!(
        body["contents"][1]["parts"][1]["inlineData"]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBOR")
    );
}

#[test]
fn test_gemini_grounding_metadata_projection() {
    let mock_gemini_response = serde_json::json!({
        "candidates": [
            {
                "content": {
                    "parts": [
                        {
                            "thought": true,
                            "text": "I need to check the official Rust edition 2024 guide."
                        },
                        {
                            "text": "According to the official announcement, Rust 2024 is the latest edition."
                        }
                    ],
                    "role": "model"
                },
                "finishReason": "STOP",
                "groundingMetadata": {
                    "webSearchQueries": [
                        "Rust 2024 edition release announcement"
                    ],
                    "groundingChunks": [
                        {
                            "web": {
                                "uri": "https://blog.rust-lang.org/2024/edition.html",
                                "title": "Rust 2024 Released"
                            }
                        }
                    ],
                    "groundingSupports": [
                        {
                            "segment": {
                                "startIndex": 0,
                                "endIndex": 40,
                                "text": "According to the official announcement"
                            },
                            "groundingChunkIndices": [0]
                        }
                    ]
                },
                "safetyRatings": [
                    {
                        "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                        "probability": "NEGLIGIBLE"
                    }
                ]
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 200,
            "candidatesTokenCount": 85,
            "totalTokenCount": 285
        }
    });

    let parsed =
        GeminiAccumulator::parse_response(&mock_gemini_response, &[]).expect("parse response");
    let msg = &parsed.messages[0];

    // 验证结构体解析：包含了 WebSearchCall, Reasoning, Text (with Annotation)
    assert_eq!(msg.parts.len(), 3);

    // 1. 验证 WebSearchCall 统一建模
    match &msg.parts[0] {
        ContentPart::WebSearchCall(call) => {
            assert_eq!(call.query, "Rust 2024 edition release announcement");
            assert_eq!(call.sources.len(), 1);
            assert_eq!(
                call.sources[0].url,
                "https://blog.rust-lang.org/2024/edition.html"
            );
            assert_eq!(call.sources[0].title, "Rust 2024 Released");
        }
        _ => panic!("Expected WebSearchCall"),
    }

    // 2. 验证 thought 映射为 Reasoning
    match &msg.parts[1] {
        ContentPart::Reasoning { content, .. } => {
            assert!(content.contains("official Rust edition"));
        }
        _ => panic!("Expected Reasoning"),
    }

    // 3. 验证文本与 UrlCitation 注脚投影
    match &msg.parts[2] {
        ContentPart::Text { text, annotations } => {
            assert!(text.contains("Rust 2024 is the latest edition"));
            assert_eq!(annotations.len(), 1);
            if let Annotation::UrlCitation {
                url,
                start_index,
                end_index,
                ..
            } = &annotations[0]
            {
                assert_eq!(url, "https://blog.rust-lang.org/2024/edition.html");
                assert_eq!(*start_index, 0);
                assert_eq!(*end_index, 40);
            } else {
                panic!("Expected UrlCitation");
            }
        }
        _ => panic!("Expected Text"),
    }

    // 验证元数据
    let gemini_meta = parsed.meta.gemini.unwrap();
    assert_eq!(gemini_meta.finish_reason.as_deref(), Some("STOP"));
    assert_eq!(gemini_meta.safety_ratings.len(), 1);
}

#[test]
fn test_gemini_system_instruction_and_tool_call_response_mapping() {
    let provider = GeminiProvider::new("test-key")
        .with_base_url("https://generativelanguage.googleapis.com/v1beta/models");
    let mut session = Session::new(PathBuf::from("/tmp"), None);

    // System 指令
    session.append_turn(Turn::new(
        Role::System,
        vec![Message::text("You are a helpful assistant.")],
    ));
    // User 问题
    session.append_turn(Turn::user_turn(Message::text("List files")));

    // Assistant 调用 bash 工具
    let asst_msg = Message::new(vec![ContentPart::ToolCall(ToolCall::new(
        "call_abc123",
        "bash",
        r#"{"command":"ls"}"#,
    ))]);
    session.append_turn(Turn::assistant_turn(vec![asst_msg]));

    // Tool 返回执行结果
    session.append_turn(Turn::new(
        Role::Tool,
        vec![Message::tool_result(ToolResult::success(
            "call_abc123",
            "file1.txt\nfile2.txt",
        ))],
    ));

    let options = CompletionOptions {
        model: "gemini-2.0-flash".into(),
        ..Default::default()
    };
    let body = provider
        .build_request_body(&session, &[], &options)
        .expect("build body");

    // 验证 systemInstruction 独立提取
    let sys_inst = &body["systemInstruction"]["parts"][0]["text"];
    assert_eq!(sys_inst, "You are a helpful assistant.");

    // 验证 contents 角色交替与结构：user -> model (functionCall) -> user (functionResponse)
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 3);
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[2]["role"], "user");

    // 验证 model 的 functionCall
    let fc = &contents[1]["parts"][0]["functionCall"];
    assert_eq!(fc["name"], "bash");
    assert_eq!(fc["args"]["command"], "ls");

    // 验证 user 的 functionResponse 函数名正确匹配为 bash，而非 call_abc123
    let fr = &contents[2]["parts"][0]["functionResponse"];
    assert_eq!(fr["name"], "bash");
    assert_eq!(fr["response"]["content"], "file1.txt\nfile2.txt");
}

#[test]
fn test_gemini_streaming_accumulator_matches_non_streaming_parse() {
    // 流式累积器与非流式解析共用 GeminiAccumulator：
    // 相同的 response 结构分多个 chunk 合并后，结果必须与一次性解析一致
    let chunk1 = serde_json::json!({
        "candidates": [
            {
                "content": {
                    "parts": [
                        { "thought": true, "text": "Analyzing " },
                        { "text": "Rust 2024 " }
                    ]
                }
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 10,
            "totalTokenCount": 110
        }
    });
    let chunk2 = serde_json::json!({
        "candidates": [
            {
                "content": {
                    "parts": [
                        { "text": "is the latest edition." }
                    ]
                },
                "finishReason": "STOP",
                "groundingMetadata": {
                    "webSearchQueries": ["rust edition"],
                    "groundingChunks": [
                        { "web": { "uri": "https://blog.rust-lang.org", "title": "Rust Blog" } }
                    ],
                    "groundingSupports": [
                        {
                            "segment": { "startIndex": 0, "endIndex": 10 },
                            "groundingChunkIndices": [0]
                        }
                    ]
                }
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 25,
            "totalTokenCount": 125
        }
    });

    let mut streaming = GeminiAccumulator::new(None, &[]);
    streaming.merge_response(&chunk1).expect("merge chunk1");
    streaming.merge_response(&chunk2).expect("merge chunk2");
    let streamed = streaming.finish();
    let (stream_parts, stream_meta, stream_usage) = (
        streamed.messages[0].parts.clone(),
        streamed.meta,
        streamed.usage,
    );

    let full = serde_json::json!({
        "candidates": [
            {
                "content": {
                    "parts": [
                        { "thought": true, "text": "Analyzing " },
                        { "text": "Rust 2024 is the latest edition." }
                    ]
                },
                "finishReason": "STOP",
                "groundingMetadata": {
                    "webSearchQueries": ["rust edition"],
                    "groundingChunks": [
                        { "web": { "uri": "https://blog.rust-lang.org", "title": "Rust Blog" } }
                    ],
                    "groundingSupports": [
                        {
                            "segment": { "startIndex": 0, "endIndex": 10 },
                            "groundingChunkIndices": [0]
                        }
                    ]
                }
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 25,
            "totalTokenCount": 125
        }
    });
    let parsed = GeminiAccumulator::parse_response(&full, &[]).expect("parse full");
    let (full_parts, full_meta, full_usage) = {
        let msg = &parsed.messages[0];
        let meta = parsed.meta.clone();
        (msg.parts.clone(), meta, parsed.usage.clone())
    };

    // 检索调用 id 含随机 UUID，比较前先规范化
    let normalize = |mut parts: Vec<ContentPart>| {
        for part in &mut parts {
            if let ContentPart::WebSearchCall(call) = part {
                call.id = "normalized".to_string();
            }
        }
        parts
    };
    assert_eq!(normalize(stream_parts), normalize(full_parts));
    assert_eq!(stream_meta, full_meta);
    assert_eq!(stream_usage, full_usage);
}

#[test]
fn test_gemini_streaming_ignores_usage_metadata_without_token_counts() {
    let intermediate = serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "A"}]}
        }],
        "usageMetadata": {"trafficType": "ON_DEMAND"}
    });
    let complete = serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "B"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 7,
            "candidatesTokenCount": 2,
            "totalTokenCount": 9,
            "trafficType": "ON_DEMAND"
        }
    });

    let mut streaming = GeminiAccumulator::new(None, &[]);
    streaming
        .merge_response(&intermediate)
        .expect("ignore intermediate usage metadata");
    streaming
        .merge_response(&complete)
        .expect("merge complete usage metadata");
    let response = streaming.finish();
    let usage = response.usage.expect("complete token usage");
    assert_eq!(usage.prompt_tokens, 7);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(usage.total_tokens, 9);

    let err = GeminiAccumulator::parse_response(&intermediate, &[])
        .expect_err("non-streaming usage metadata remains strict");
    assert!(matches!(
        err,
        ProviderError::Protocol(msg) if msg.contains("Missing 'promptTokenCount'")
    ));
}

#[test]
fn test_gemini_streaming_grounding_chunks_merge_not_overwrite() {
    // 回归：流式响应中多个 chunk 分别携带 groundingMetadata 时，
    // 检索来源与查询必须合并保留，不得整体覆盖
    let chunk1 = serde_json::json!({
        "candidates": [{
            "groundingMetadata": {
                "webSearchQueries": ["query one"],
                "groundingChunks": [
                    { "web": { "uri": "https://a.example.com", "title": "A" } }
                ]
            }
        }]
    });
    let chunk2 = serde_json::json!({
        "candidates": [{
            "groundingMetadata": {
                "webSearchQueries": ["query two"],
                "groundingChunks": [
                    { "web": { "uri": "https://b.example.com", "title": "B" } }
                ]
            }
        }]
    });

    let mut acc = GeminiAccumulator::new(None, &[]);
    acc.merge_response(&chunk1).expect("merge chunk1");
    acc.merge_response(&chunk2).expect("merge chunk2");
    let parts = acc.finish().messages.remove(0).parts;

    match &parts[0] {
        ContentPart::WebSearchCall(call) => {
            assert_eq!(call.sources.len(), 2);
            assert_eq!(call.sources[0].url, "https://a.example.com");
            assert_eq!(call.sources[1].url, "https://b.example.com");
            assert!(call.query.contains("query one"));
            assert!(call.query.contains("query two"));
        }
        _ => panic!("Expected WebSearchCall"),
    }
}

#[test]
fn test_file_ref_projects_as_text_part() {
    let provider = GeminiProvider::new("test");
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::new(vec![
        ContentPart::text("解释 @src/main.rs"),
        ContentPart::FileRef {
            path: "src/main.rs".into(),
            content: "fn main() {}".into(),
            truncated: false,
        },
    ])));

    let body = provider
        .build_request_body(&session, &[], &CompletionOptions::default())
        .expect("build body");
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["text"], "解释 @src/main.rs");
    assert_eq!(parts[1]["text"], "@src/main.rs\nfn main() {}");
}

#[test]
fn test_response_schema_maps_to_generation_config() {
    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("route")));
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"agent": {"type": "string"}},
        "required": ["agent"],
        "additionalProperties": false
    });
    let options = CompletionOptions {
        model: "gemini-2.5-flash".into(),
        response_schema: Some(crate::provider::ResponseSchema {
            name: "route_decision".into(),
            schema: schema.clone(),
        }),
        ..Default::default()
    };
    let body = GeminiProvider::new("k")
        .build_request_body(&session, &[], &options)
        .unwrap();
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(body["generationConfig"]["responseJsonSchema"], schema);
}

fn freeform_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "apply_patch".into(),
        description: "apply a patch".into(),
        input: ToolInput::Freeform { grammar: None },
    }]
}

#[test]
fn test_gemini_freeform_tool_projection() {
    let provider = GeminiProvider::new("test");
    let tools = freeform_tools();

    let mut session = Session::new(PathBuf::from("/tmp"), None);
    session.append_turn(Turn::user_turn(Message::text("go")));
    session.append_turn(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::ToolCall(ToolCall::custom(
            "call_1",
            "apply_patch",
            "*** Begin Patch\n*** End Patch",
        )),
    ])]));
    let body = provider
        .build_request_body(&session, &tools, &CompletionOptions::default())
        .expect("build body");

    // 声明：单字段 input 的 parameters
    let decls = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|t| t.get("functionDeclarations"))
        .expect("functionDeclarations");
    assert_eq!(
        decls[0]["parameters"]["properties"]["input"]["type"],
        "string"
    );
    assert_eq!(
        decls[0]["parameters"]["required"],
        serde_json::json!(["input"])
    );

    // 回放：Custom 调用包装为 {"input": 原文}
    assert_eq!(
        body["contents"][1]["parts"][0]["functionCall"]["args"],
        serde_json::json!({"input": "*** Begin Patch\n*** End Patch"})
    );

    // 解析：还原为 Custom 调用原文
    let resp = |args: serde_json::Value| {
        serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "apply_patch", "args": args}}
                ]},
                "finishReason": "STOP"
            }]
        })
    };
    let parsed =
        GeminiAccumulator::parse_response(&resp(serde_json::json!({"input": "raw text"})), &tools)
            .expect("parse");
    let ContentPart::ToolCall(call) = &parsed.messages[0].parts[0] else {
        panic!("expected tool call");
    };
    assert_eq!(call.kind, ToolCallKind::Custom);
    assert_eq!(call.name, "apply_patch");
    assert_eq!(call.arguments, "raw text");

    // 缺 input 显式报错
    let err = GeminiAccumulator::parse_response(&resp(serde_json::json!({"patch": "x"})), &tools)
        .expect_err("error");
    assert!(matches!(err, ProviderError::Protocol(_)));
}
