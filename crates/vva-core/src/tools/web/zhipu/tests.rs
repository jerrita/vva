use super::*;
use crate::test_support::http::{Response, serve};
use serde_json::{Value, json};
use tokio::sync::mpsc;

// 报文结构来自智谱 Web Search API 文档；内容为合成数据。
fn hits() -> Value {
    json!([
        {"title":"Example guide", "link":"https://example.com/guide", "content":"Guide content", "media":"Example", "publish_date":"2026-01-01"},
        {"title":"Example reference", "link":"https://example.com/reference", "content":"Reference content"}
    ])
}

#[tokio::test]
async fn rest_sends_native_engines_and_limits_results() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (base, server) = serve(move |request| {
        let tx = tx.clone();
        async move {
            assert_eq!(request.path, "/search");
            assert_eq!(request.headers["authorization"], "Bearer test-key");
            tx.send(request.body).unwrap();
            Response::json(json!({"search_result": hits()}))
        }
    })
    .await;
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
        let provider = ZhipuSearchProvider::new("test-key".into(), engine, client_info());
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
    let provider = ZhipuSearchProvider::new(
        "test-key".into(),
        ZhipuSearchEngine::SearchStd,
        client_info(),
    );
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
    let (base, server) = serve(|request| async move {
        match request.path.as_str() {
            "/http" => Response::status(429, "test-key"),
            "/api" => Response::json(json!({"error":{"code":"1302", "message":"test-key"}})),
            "/invalid" => Response::json(json!({})),
            "/empty" => Response::json(json!({"search_result":[]})),
            other => panic!("unexpected path: {other}"),
        }
    })
    .await;
    let provider = ZhipuSearchProvider::new(
        "test-key".into(),
        ZhipuSearchEngine::SearchStd,
        client_info(),
    );
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
    let (base, server) = serve(move |request| {
        let tx = tx.clone();
        let options = options.clone();
        async move {
            let headers = request.headers;
            if request.method == "DELETE" {
                assert_eq!(headers["mcp-session-id"], "test-session");
                tx.send(json!({"method":"DELETE"})).unwrap();
                return Response::status(405, "");
            }
            let body = request.body;
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
                "notifications/initialized" => return Response::status(202, ""),
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
                        Some("http") => return Response::status(401, "test-key"),
                        Some("rpc") => return Response::json(json!({"jsonrpc":"2.0", "id":body["id"], "error":{"code":-32603,"message":"test-key"}})),
                        Some("pending") => return std::future::pending::<Response>().await,
                        _ => json!({"content":[{"type":"text", "text":serde_json::to_string(&hits().to_string()).unwrap()}], "isError":options.failure == Some("tool")}),
                    }
                }
                other => panic!("unexpected method: {other}"),
            };
            let reply = json!({"jsonrpc":"2.0", "id":body["id"], "result":result});
            let mut response = if options.sse { Response { status: 200,
                    headers: vec![("content-type", "text/event-stream;charset=UTF-8".into())],
                    body: format!("event: message\ndata: {reply}\n\n"), keep_open: true,
                } } else { Response::json(reply) };
            if body["method"] == "initialize" {
                response.headers.push(("Mcp-Session-Id", "test-session".into()));
            }
            response
        }
    }).await;
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
            mcp::search(&endpoint, "test-key", "example", 1, &client_info()),
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
        let error = mcp::search(&endpoint, "test-key", "example", 1, &client_info())
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
    let task = tokio::spawn(async move {
        mcp::search(&endpoint, "test-key", "example", 1, &client_info()).await
    });
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

fn client_info() -> ClientInfo {
    ClientInfo {
        name: "test".into(),
        version: "1".into(),
    }
}
