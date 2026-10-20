use super::*;
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;

// 报文结构来自智谱 Web Search API 文档；内容为合成数据。
fn hits() -> Value {
    json!([
        {"title":"Example guide", "link":"https://example.com/guide", "content":"Guide content", "media":"Example", "publish_date":"2026-01-01"},
        {"title":"Example reference", "link":"https://example.com/reference", "content":"Reference content"}
    ])
}

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (endpoint, server)
}

#[tokio::test]
async fn rest_sends_native_engines_and_limits_results() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let app = Router::new().route(
        "/search",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let tx = tx.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer test-key");
                tx.send(body).unwrap();
                Json(json!({"search_result": hits()}))
            }
        }),
    );
    let (base, server) = serve(app).await;
    for (engine, serialized, count) in [
        (ZhipuSearchEngine::SearchStd, "search_std", Some(3)),
        (ZhipuSearchEngine::SearchPro, "search_pro", Some(3)),
        (
            ZhipuSearchEngine::SearchProSogou,
            "search_pro_sogou",
            Some(10),
        ),
        (ZhipuSearchEngine::SearchProQuark, "search_pro_quark", None),
    ] {
        let provider = ZhipuSearchProvider::new("test-key".into(), engine);
        let results = provider
            .rest_search(&format!("{base}/search"), "example", 3)
            .await
            .unwrap();
        let mut expected =
            json!({"search_query":"example", "search_engine":serialized, "search_intent":false});
        if let Some(count) = count {
            expected["count"] = count.into();
        }
        assert_eq!(rx.recv().await.unwrap(), expected);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://example.com/guide");
        assert_eq!(results[1].snippet, "Reference content");
    }
    let provider = ZhipuSearchProvider::new("test-key".into(), ZhipuSearchEngine::SearchStd);
    assert_eq!(
        provider
            .rest_search(&format!("{base}/search"), "example", 1)
            .await
            .unwrap()
            .len(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn rest_reports_http_api_and_malformed_responses() {
    let app = Router::new()
        .route(
            "/http",
            post(|| async { (StatusCode::TOO_MANY_REQUESTS, "test-key") }),
        )
        .route(
            "/api",
            post(|| async { Json(json!({"error":{"code":"1302", "message":"test-key"}})) }),
        )
        .route("/invalid", post(|| async { Json(json!({})) }))
        .route(
            "/empty",
            post(|| async { Json(json!({"search_result":[]})) }),
        );
    let (base, server) = serve(app).await;
    let provider = ZhipuSearchProvider::new("test-key".into(), ZhipuSearchEngine::SearchStd);
    for (path, expected) in [
        ("http", "429"),
        ("api", "1302"),
        ("invalid", "missing search_result"),
    ] {
        let error = provider
            .rest_search(&format!("{base}/{path}"), "example", 1)
            .await
            .unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("test-key"));
    }
    assert!(
        provider
            .rest_search(&format!("{base}/empty"), "example", 1)
            .await
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[derive(Clone, Default)]
struct McpOptions {
    sse: bool,
    tool_name: &'static str,
    failure: Option<&'static str>,
}

async fn mcp_backend(
    options: McpOptions,
) -> (
    String,
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<Value>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cleanup = tx.clone();
    let app = Router::new().route("/mcp", post(move |headers: HeaderMap, Json(body): Json<Value>| {
        let tx = tx.clone();
        let options = options.clone();
        async move {
            assert_eq!(headers["authorization"], "Bearer test-key");
            assert!(headers["accept"].to_str().unwrap().contains("text/event-stream"));
            assert_eq!(body["jsonrpc"], "2.0");
            tx.send(body.clone()).unwrap();
            if body["method"] != "initialize" {
                assert_eq!(headers["mcp-session-id"], "test-session");
                assert_eq!(headers["mcp-protocol-version"], "2024-11-05");
            }
            let result = match body["method"].as_str().unwrap() {
                "initialize" => json!({"protocolVersion":"2024-11-05", "capabilities":{"tools":{}}, "serverInfo":{"name":"test-search", "version":"1"}}),
                "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
                "tools/list" => {
                    if body["params"]["cursor"].is_null() {
                        json!({"tools":[], "nextCursor":"next"})
                    } else {
                        assert_eq!(body["params"]["cursor"], "next");
                        json!({"tools":[{"name":options.tool_name, "inputSchema":{"type":"object", "properties":{"search_query":{"type":"string"}}, "required":["search_query"]}}]})
                    }
                }
                "tools/call" => {
                    assert_eq!(body["params"], json!({"name":options.tool_name, "arguments":{"search_query":"example"}}));
                    match options.failure {
                        Some("http") => return (StatusCode::UNAUTHORIZED, "test-key").into_response(),
                        Some("rpc") => return Json(json!({"jsonrpc":"2.0", "id":body["id"], "error":{"code":-32603,"message":"test-key"}})).into_response(),
                        Some("pending") => return std::future::pending::<Response>().await,
                        _ => json!({"content":[{"type":"text", "text":serde_json::to_string(&hits().to_string()).unwrap()}], "isError":options.failure == Some("tool")}),
                    }
                }
                other => panic!("unexpected method: {other}"),
            };
            let reply = json!({"jsonrpc":"2.0", "id":body["id"], "result":result});
            let mut response = if options.sse {
                // 保持连接打开，验证客户端收到匹配响应后立即返回。
                let chunks = futures::stream::once(async move {
                    Ok::<_, std::convert::Infallible>(format!("event: message\ndata: {reply}\n\n"))
                }).chain(futures::stream::pending());
                ([ ("content-type", "text/event-stream;charset=UTF-8") ], axum::body::Body::from_stream(chunks)).into_response()
            } else {
                Json(reply).into_response()
            };
            if body["method"] == "initialize" {
                response.headers_mut().insert("Mcp-Session-Id", "test-session".parse().unwrap());
            }
            response
        }
    }).delete(move |headers: HeaderMap| {
        let cleanup = cleanup.clone();
        async move {
            assert_eq!(headers["mcp-session-id"], "test-session");
            cleanup.send(json!({"method":"DELETE"})).unwrap();
            StatusCode::METHOD_NOT_ALLOWED
        }
    }));
    let (base, server) = serve(app).await;
    (format!("{base}/mcp"), server, rx)
}

async fn await_cleanup(rx: &mut mpsc::UnboundedReceiver<Value>) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while rx.recv().await.unwrap()["method"] != "DELETE" {}
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn mcp_negotiates_json_and_sse_discovers_tools_and_cleans_up() {
    for (sse, tool_name) in [(false, "webSearchPrime"), (true, "web_search_prime")] {
        let (endpoint, server, mut rx) = mcp_backend(McpOptions {
            sse,
            tool_name,
            failure: None,
        })
        .await;
        let results = tokio::time::timeout(
            Duration::from_secs(3),
            mcp::search(&endpoint, "test-key", "example", 1),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example guide");
        assert_eq!(results[0].snippet, "Guide content");
        await_cleanup(&mut rx).await;
        server.abort();
    }
}

#[tokio::test]
async fn mcp_errors_and_cancellation_terminate_session() {
    for (failure, expected) in [
        ("http", "401"),
        ("rpc", "-32603"),
        ("tool", "tool returned an error"),
    ] {
        let (endpoint, server, mut rx) = mcp_backend(McpOptions {
            tool_name: "webSearchPrime",
            failure: Some(failure),
            ..Default::default()
        })
        .await;
        let error = mcp::search(&endpoint, "test-key", "example", 1)
            .await
            .unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("test-key"));
        await_cleanup(&mut rx).await;
        server.abort();
    }
    let (endpoint, server, mut rx) = mcp_backend(McpOptions {
        tool_name: "webSearchPrime",
        failure: Some("pending"),
        ..Default::default()
    })
    .await;
    let task = tokio::spawn(async move { mcp::search(&endpoint, "test-key", "example", 1).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while rx.recv().await.unwrap()["method"] != "tools/call" {}
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    await_cleanup(&mut rx).await;
    server.abort();
}
