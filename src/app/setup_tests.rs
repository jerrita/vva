use super::*;
use crate::app::model::Session;
use vva_core::provider::ProviderError;
use vva_core::provider::ProviderResponse;
use vva_core::tools::names;

#[tokio::test]
async fn title_generation_disables_thinking_with_native_parameters() {
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};

    let cases = [
        (
            "response",
            json!({
                "id": "resp_title", "status": "completed",
                "output": [{
                    "type": "message", "id": "msg_title", "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "整理项目文档", "annotations": []}]
                }]
            }),
            "/reasoning",
            json!({"effort": "none"}),
        ),
        (
            "anthropic",
            json!({
                "id": "msg_title", "type": "message", "role": "assistant",
                "model": "title-model",
                "content": [{"type": "text", "text": "整理项目文档"}],
                "stop_reason": "end_turn", "stop_sequence": null,
                "usage": {"input_tokens": 50, "output_tokens": 6}
            }),
            "/thinking",
            json!({"type": "disabled"}),
        ),
        (
            "gemini",
            json!({
                "candidates": [{
                    "content": {"role": "model", "parts": [{"text": "整理项目文档"}]},
                    "finishReason": "STOP"
                }]
            }),
            "/generationConfig/thinkingConfig",
            json!({"thinkingBudget": 0}),
        ),
    ];
    for (protocol, response, parameter, disabled) in cases {
        for reasoning in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (requests, mut received) = mpsc::channel(1);
            let response = response.clone();
            let app = Router::new().fallback(post(move |Json(body): Json<Value>| {
                let requests = requests.clone();
                let response = response.clone();
                async move {
                    requests.send(body).await.unwrap();
                    Json(response)
                }
            }));
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let config: AppConfig = serde_json::from_value(json!({
                    "settings": {"small_model": "test/title-model"},
                    "providers": {"test": {
                        "type": "response", "base_url": format!("http://{address}"),
                        "api_key": "test-key",
                        "models": {"title-model": {"name": "Title", "type": protocol, "reasoning": reasoning,
                            "annotation": {"websocket": false}}}
                    }}
                })).unwrap();
            let result = generate_title(&config, "unused/model", "整理项目文档").await;
            server.abort();
            assert_eq!(result.unwrap(), "整理项目文档");
            let body = received.try_recv().unwrap();
            if reasoning {
                assert_eq!(body.pointer(parameter), Some(&disabled), "{protocol}");
            } else {
                assert!(body.pointer(parameter).is_none(), "{protocol}");
            }
        }
    }
}

/// 无原生搜索能力的供应商桩：覆盖能力协商的降级通知分支
struct NoSearchProvider;

#[async_trait::async_trait]
impl Provider for NoSearchProvider {
    fn name(&self) -> &str {
        "no-search"
    }

    fn capabilities(&self) -> vva_core::provider::ProviderCapabilities {
        vva_core::provider::ProviderCapabilities {
            supports_native_search: false,
            supports_reasoning_effort: false,
            server_tool_names: Vec::new(),
        }
    }

    async fn complete(
        &self,
        _session: &vva_core::model::Session,
        _tools: &[vva_core::model::ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<ProviderResponse, ProviderError> {
        unimplemented!()
    }

    async fn complete_stream(
        &self,
        _session: &vva_core::model::Session,
        _tools: &[vva_core::model::ToolDefinition],
        _options: &CompletionOptions,
        _event_sender: Option<tokio::sync::mpsc::Sender<vva_core::event::AgentEvent>>,
    ) -> Result<ProviderResponse, ProviderError> {
        unimplemented!()
    }
}

#[test]
fn test_capability_notices_flag_unsupported_options() {
    // 支持原生搜索和推理档位的供应商无通知
    let options = CompletionOptions {
        model: "claude-x".into(),
        enable_web_search: true,
        reasoning_effort: Some("high".into()),
        ..Default::default()
    };
    let response = vva_core::provider::response::ResponseProvider::new("k");
    assert!(capability_notices(&response, &options, false).is_empty());
    let anthropic = vva_core::provider::anthropic::AnthropicProvider::new("k");
    assert!(capability_notices(&anthropic, &options, false).is_empty());

    // 无原生搜索能力的供应商生成搜索降级通知
    let search_only = CompletionOptions {
        model: "claude-x".into(),
        enable_web_search: true,
        ..Default::default()
    };
    let notices = capability_notices(&NoSearchProvider, &search_only, false);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("native search"));
    assert!(notices[0].contains("no-search"));

    // 外置搜索已接管时不再报"选项被忽略"
    assert!(capability_notices(&NoSearchProvider, &search_only, true).is_empty());

    // 不支持推理档位时单独提示
    let effort_only = CompletionOptions {
        model: "claude-x".into(),
        enable_web_search: false,
        reasoning_effort: Some("high".into()),
        ..Default::default()
    };
    let notices = capability_notices(&NoSearchProvider, &effort_only, false);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("'high'"));
}

/// 应用装配的测试构造器：不发起网络请求
fn build_engine(config: &AppConfig, session: &Session) -> Result<ExecutionPlan, String> {
    {
        let mut agents = crate::agents::builtin_agents();
        if session.header.agent_managed() {
            agents.push(crate::space::curator::curator_agent());
        }
        build_engine_with_agents(config, session, agents)
    }
}

fn build_engine_with_agents(
    config: &AppConfig,
    session: &Session,
    agents: Vec<vva_core::agents::AgentDef>,
) -> Result<ExecutionPlan, String> {
    let (tx, _rx) = mpsc::channel(1);
    VvaSessionSetup::assemble(
        config,
        &session.to_core(),
        EngineDeps {
            provider: Arc::default(),
            tool_outputs: std::env::temp_dir()
                .join(format!("vva-agent-test-{}", uuid::Uuid::new_v4())),
            hashline: vva_core::tools::hashline::HashlineSession::new(),
            ask_user_requests: tx,
            background: Arc::new(vva_core::tools::background::BackgroundTasks::new(Arc::new(
                |_| {},
            ))),
            shell_outputs: Arc::default(),
            skills: Vec::new(),
            subagents: agents
                .iter()
                .filter(|def| def.name != crate::space::curator::CURATOR_AGENT)
                .cloned()
                .collect(),
            agents,
            broker: Arc::new(crate::test_support::broker::StubBroker(None)),
            extra_tools: Vec::new(),
            interactive: !session.header.agent_managed(),
            coding: crate::app::records::CodingSessionConfig {
                editor: session.header.editor,
                web_search: session.header.web_search,
            },
        },
    )
}

#[tokio::test]
async fn ask_user_is_absent_for_agent_managed_sessions() {
    let config = web_config(None);
    let plain = build_engine(&config, &session_for("p/nos", false)).unwrap();
    assert!(plain.tools.get("AskUser").is_some());

    let mut managed = session_for("p/nos", false);
    managed.header.origin = Some(crate::app::records::Origin {
        channel: "qq".into(),
        chat_id: "c2c:1".into(),
    });
    let engine = build_engine(&config, &managed).unwrap();
    assert!(engine.tools.get("AskUser").is_none());

    let mut curator = session_for("p/nos", false);
    curator.header.kind = crate::app::records::SessionKind::Curator;
    assert!(
        build_engine(&config, &curator)
            .unwrap()
            .tools
            .get("AskUser")
            .is_none()
    );
}

#[tokio::test]
async fn engine_registers_subagent_tools_for_top_level_sessions() {
    let config = web_config(None);
    let engine = build_engine(&config, &session_for("p/nos", false)).unwrap();

    assert!(engine.tools.get("Agent").is_some());
    assert!(engine.tools.get("SendMessage").is_some());
    // agent 参数 enum 来自发现结果（含内置定义）
    let agent = engine.tools.get("Agent").unwrap();
    let vva_core::model::ToolInput::Json(parameters) = agent.input() else {
        panic!("expected JSON input");
    };
    let names: Vec<_> = parameters["properties"]["agent"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(names, ["explore", "general"]);
}

#[tokio::test]
async fn subagent_session_registry_is_pruned_to_def_allowlist() {
    let config = web_config(None);
    let mut session = session_for("p/nos", false);
    session.header.agent = Some("explore".into());
    session.header.parent = Some("sess_p0".into());
    session.header.depth = 1;
    let engine = build_engine(&config, &session).unwrap();

    // 缺省 patch 编辑协议：只读白名单保留该协议的读取与检索工具
    assert!(engine.tools.get(names::READ).is_some());
    assert!(engine.tools.get(names::GLOB).is_some());
    assert!(engine.tools.get(names::GREP).is_some());
    for pruned in [names::SHELL, names::APPLY_PATCH, "Agent", "SendMessage"] {
        assert!(
            engine.tools.get(pruned).is_none(),
            "{pruned} should be pruned"
        );
    }
}

#[tokio::test]
async fn subagent_missing_definition_is_rejected() {
    let config = web_config(None);
    let mut session = session_for("p/nos", true);
    session.header.agent = Some("missing-agent".into());
    let Err(error) = build_engine(&config, &session) else {
        panic!("missing definition was accepted");
    };
    assert!(error.contains("missing-agent"));
}

#[tokio::test]
async fn subagent_web_permissions_cover_native_and_external_tools() {
    for provider_type in [
        vva_core::provider::ProviderType::Response,
        vva_core::provider::ProviderType::Anthropic,
        vva_core::provider::ProviderType::Gemini,
    ] {
        for (native, standalone) in [(true, false), (false, false), (true, true), (false, true)] {
            for (search, fetch) in [(false, false), (true, false), (false, true), (true, true)] {
                let mut config = web_config(Some(crate::config::model::ModelAnnotation {
                    web_search: Some(native),
                    standalone_search: Some(standalone),
                    ..Default::default()
                }));
                config.providers.get_mut("p").unwrap().provider_type = Some(provider_type);
                let mut session = session_for("p/nos", true);
                session.header.agent = Some("restricted".into());
                let mut allowed = vec![names::READ.to_string()];
                if search {
                    allowed.push("WebSearch".into());
                }
                if fetch {
                    allowed.push("WebFetch".into());
                }
                let defs = vec![vva_core::agents::AgentDef {
                    name: "restricted".into(),
                    description: "Restricted agent".into(),
                    model: None,
                    tools: allowed,
                    system_prompt: None,
                }];
                let engine = build_engine_with_agents(&config, &session, defs).unwrap();
                if !search {
                    assert!(!engine.options.enable_web_search);
                    assert!(engine.tools.get("web_search").is_none());
                    assert!(!engine.tools.is_server_tool("web_search"));
                }
                if !fetch {
                    assert!(!engine.options.enable_web_fetch);
                    assert!(engine.tools.get("web_fetch").is_none());
                    assert!(!engine.tools.is_server_tool("web_fetch"));
                }
                if !native {
                    assert_eq!(engine.tools.get("web_search").is_some(), search);
                    assert_eq!(engine.tools.get("web_fetch").is_some(), fetch);
                }
                if native && provider_type == vva_core::provider::ProviderType::Anthropic {
                    assert_eq!(engine.options.enable_web_search, search && !standalone);
                    assert_eq!(
                        engine.tools.get("web_search").is_some(),
                        search && standalone
                    );
                    assert_eq!(engine.options.enable_web_fetch, fetch);
                }
            }
        }
    }
}

#[tokio::test]
async fn standalone_search_returns_text_without_replaying_server_blocks() {
    use axum::{Json, Router, http::HeaderMap, response::IntoResponse, routing::post};
    use serde_json::{Value, json};

    let events = [
        json!({"type": "message_start", "message": {
            "id": "msg_search", "type": "message", "role": "assistant", "model": "search-model",
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 1, "output_tokens": 0}
        }}),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "server_tool_use", "id": "call_search", "name": "web_search", "input": {}
        }}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": "{\"query\":\"example documentation\"}"
        }}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {
            "type": "web_search_tool_result", "tool_use_id": "different_result_id", "content": [
                {"type": "web_search_result", "title": "Example docs", "url": "https://example.com/docs", "encrypted_content": "opaque_1"},
                {"type": "web_search_result", "url": "https://example.com/guide", "encrypted_content": "opaque_2"}
            ]
        }}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "Search summary."}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 20}}),
        json!({"type": "message_stop"}),
    ];
    let sse: String = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (requests, mut received) = mpsc::channel(8);
    let app = Router::new().route("/v1/messages", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let requests = requests.clone();
            let sse = sse.clone();
            async move {
                assert_eq!(headers["x-api-key"], "fixture-key");
                assert_eq!(headers["anthropic-version"], "2023-06-01");
                requests.send(body.clone()).await.unwrap();
                if body["tool_choice"]["name"] == "web_search" {
                    return ([("content-type", "text/event-stream")], sse).into_response();
                }
                let has_result = body["messages"].as_array().unwrap().iter().any(|message| {
                    message["content"].as_array().unwrap().iter().any(|block| block["type"] == "tool_result")
                });
                let (content, stop_reason) = if has_result {
                    (json!([{"type": "text", "text": "Answer with sources."}]), "end_turn")
                } else {
                    (json!([{"type": "tool_use", "id": "client_search", "name": "web_search", "input": {"query": "example documentation"}}]), "tool_use")
                };
                Json(json!({
                    "id": "msg_main", "type": "message", "role": "assistant", "model": "search-model",
                    "content": content, "stop_reason": stop_reason, "stop_sequence": null,
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                })).into_response()
            }
        }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for (version, expected_tool) in [
        (None, "web_search_20260318"),
        (Some("20250305"), "web_search_20250305"),
    ] {
        let config: AppConfig = serde_json::from_value(json!({
            "settings": {"web": {"search": "tavily", "provider": {"tavily": {"api_key": "fixture-external-key"}}}},
            "providers": {"p": {
                "type": "anthropic", "base_url": format!("http://{address}"), "api_key": "fixture-key",
                "models": {"search-model": {"name": "Search", "annotation": {"standalone_search": true, "web_tool_version": version, "web_fetch": false}}}
            }}
        })).unwrap();
        let mut session = session_for("p/search-model", true);
        session.append_turn(Turn::user_turn(Message::text("main-only-marker")));
        let engine = build_engine(&config, &session).unwrap();
        assert!(!engine.options.enable_web_search);
        assert!(!engine.tools.is_server_tool("web_search"));
        assert!(engine.external_web.is_empty());
        let tool = engine.tools.get("web_search").unwrap();
        let vva_core::model::ToolInput::Json(input) = tool.input() else {
            panic!("expected JSON input");
        };
        assert!(input["properties"].get("max_results").is_none());
        assert!(tool.execute(r#"{"query":" "}"#).await.is_err());
        let disabled = build_engine(&config, &session_for("p/search-model", false)).unwrap();
        assert!(disabled.tools.get("web_search").is_none());

        let definitions = engine.tools.definitions();
        let response = engine
            .provider
            .complete(&session, &definitions, &engine.options)
            .await
            .unwrap();
        let vva_core::model::ContentPart::ToolCall(call) = &response.messages[0].parts[0] else {
            panic!("expected client search call");
        };
        let result = engine.tools.execute_call(call).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("Example docs"));
        assert!(result.content.contains("https://example.com/guide"));
        assert!(result.content.contains("Search summary."));
        assert!(!result.content.contains("different_result_id"));
        assert!(!result.content.contains("opaque_"));
        assert_eq!(result.tool_call_id, "client_search");
        let mut turn = Turn::assistant_turn(response.messages);
        turn.meta = response.meta;
        session.append_turn(turn);
        session.append_turn(Turn::new(
            Role::Tool,
            vec![Message::new(vec![
                vva_core::model::ContentPart::ToolResult(result),
            ])],
        ));
        let final_response = engine
            .provider
            .complete(&session, &definitions, &engine.options)
            .await
            .unwrap();
        assert_eq!(
            final_response.messages[0].text_content(),
            "Answer with sources."
        );

        let main = received.try_recv().unwrap();
        assert!(
            main["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| tool.get("type").is_none())
        );
        let search = received.try_recv().unwrap();
        assert_eq!(search["model"], "search-model");
        assert_eq!(search["thinking"], json!({"type": "disabled"}));
        assert_eq!(
            search["tool_choice"],
            json!({"type": "tool", "name": "web_search"})
        );
        assert_eq!(search["tools"].as_array().unwrap().len(), 1);
        assert_eq!(search["tools"][0]["type"], expected_tool);
        assert_eq!(search["messages"].as_array().unwrap().len(), 1);
        assert!(!search.to_string().contains("main-only-marker"));
        let resumed = received.try_recv().unwrap();
        let content: Vec<_> = resumed["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .collect();
        assert!(
            content
                .iter()
                .any(|block| block["type"] == "tool_result"
                    && block["tool_use_id"] == "client_search")
        );
        assert!(
            content
                .iter()
                .all(|block| block["type"] != "server_tool_use"
                    && block["type"] != "web_search_tool_result")
        );
    }
    server.abort();
}

fn web_config(annotation: Option<crate::config::model::ModelAnnotation>) -> AppConfig {
    let mut config = AppConfig::default();
    config.settings.web = Some(
        serde_json::from_str(
            r#"{
                "provider": {
                    "brave": { "api_key": "k-brave" },
                    "tavily": { "api_key": "k-tavily" }
                },
                "search": "tavily",
                "fetch": "local"
            }"#,
        )
        .unwrap(),
    );
    let mut models = std::collections::HashMap::new();
    models.insert(
        "nos".into(),
        crate::config::ModelConfig {
            name: "No Search".into(),
            annotation,
            ..Default::default()
        },
    );
    config.providers.insert(
        "p".into(),
        crate::config::ProviderConfig {
            api_key: "k".into(),
            models,
            ..Default::default()
        },
    );
    config
}

fn session_for(model: &str, web_search: bool) -> Session {
    let mut session = Session::new(".", None);
    session.set_model(model.to_string(), None);
    session.set_toggles(crate::app::records::SessionToggles { web_search });
    session
}

#[tokio::test]
async fn engine_registers_external_web_tools_when_native_missing() {
    // annotation.web_search: false 关闭原生搜索；fetch 随 search 一并缺位
    let annotation = crate::config::model::ModelAnnotation {
        web_search: Some(false),
        ..Default::default()
    };
    let config = web_config(Some(annotation));
    let engine = build_engine(&config, &session_for("p/nos", true)).unwrap();

    assert!(!engine.options.enable_web_search);
    assert!(engine.tools.get("web_search").is_some());
    assert!(engine.tools.get("web_fetch").is_some());
    // 客户端工具接管时不再预留原生 server tool 名
    assert!(!engine.tools.is_server_tool("web_search"));
    assert_eq!(engine.external_web, ["tavily", "local"]);
    assert!(
        !engine
            .notices
            .iter()
            .any(|notice| notice.contains("the option is ignored"))
    );
}

#[tokio::test]
async fn engine_skips_external_tools_without_native_gap_or_toggle() {
    // 原生 web_search 在位（无注记）时搜索与 fetch 均不外置：
    // Response 的 web_search 含 open_page / find_in_page
    let config = web_config(None);
    let engine = build_engine(&config, &session_for("p/nos", true)).unwrap();
    assert!(engine.options.enable_web_search);
    assert!(engine.tools.get("web_search").is_none());
    assert!(engine.tools.get("web_fetch").is_none());
    assert!(engine.tools.is_server_tool("web_search"));
    assert!(engine.notices.is_empty());
    assert!(engine.external_web.is_empty());

    // 会话 web 开关关闭时一律不注册
    let engine = build_engine(&config, &session_for("p/nos", false)).unwrap();
    assert!(!engine.options.enable_web_search);
    assert!(engine.tools.get("web_search").is_none());
    assert!(engine.tools.get("web_fetch").is_none());
    assert!(!engine.tools.is_server_tool("web_search"));
    assert!(engine.notices.is_empty());
}

#[tokio::test]
async fn engine_fails_on_web_provider_capability_mismatch() {
    let mut config = web_config(Some(crate::config::model::ModelAnnotation {
        web_search: Some(false),
        ..Default::default()
    }));
    config.settings.web.as_mut().unwrap().search = Some("local".into());
    let Err(error) = build_engine(&config, &session_for("p/nos", true)) else {
        panic!("expected engine build to fail on capability mismatch");
    };

    assert!(error.contains("only supports web fetch"), "{error}");
}

#[tokio::test]
async fn engine_notices_affinity_downgrade_on_model_switch() {
    let config = web_config(None);
    let mut session = session_for("p/nos", true);
    // 上一轮由不同亲和组产出且携带原生状态
    let mut turn = Turn::assistant_turn(vec![Message::new(vec![
        vva_core::model::ContentPart::text("earlier answer"),
    ])]);
    turn.meta.response = Some(vva_core::model::ResponseTurnMeta::new("resp_prev"));
    turn.meta.response.as_mut().unwrap().scope = Some("homo/gpt".into());
    session.append_turn(turn);

    let engine = build_engine(&config, &session).unwrap();
    assert!(
        engine
            .notices
            .iter()
            .any(|notice| notice.contains("Native context from 'homo/gpt'")),
        "{:?}",
        engine.notices
    );

    // 同亲和组续跑不提示
    let mut session = session_for("p/nos", true);
    let mut turn = Turn::assistant_turn(vec![Message::text("earlier")]);
    turn.meta.response = Some(vva_core::model::ResponseTurnMeta::new("resp_prev"));
    turn.meta.response.as_mut().unwrap().scope = Some("p".into());
    session.append_turn(turn);
    let engine = build_engine(&config, &session).unwrap();
    assert!(
        !engine
            .notices
            .iter()
            .any(|notice| notice.contains("cannot be replayed"))
    );
}
