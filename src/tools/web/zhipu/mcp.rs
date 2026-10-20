//! 专用搜索端点的 Streamable HTTP 会话；复用已有 SSE 解码器。

use std::{collections::HashSet, time::Duration};

use futures::StreamExt;
use reqwest::{Method, Response, header::HeaderValue};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{SearchHit, WebSearchResult, client, project};
use crate::provider::sse::SseDecoder;

struct Session {
    endpoint: String,
    api_key: String,
    session_id: Option<HeaderValue>,
    protocol_version: Option<String>,
    next_id: u64,
}

impl Session {
    fn request(&self, method: Method) -> reqwest::RequestBuilder {
        let mut request = client()
            .request(method, &self.endpoint)
            .bearer_auth(&self.api_key)
            .header("Accept", "application/json, text/event-stream");
        if let Some(id) = &self.session_id {
            request = request.header("Mcp-Session-Id", id);
        }
        if let Some(version) = &self.protocol_version {
            request = request.header("MCP-Protocol-Version", version);
        }
        request
    }

    async fn call<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<T, String> {
        let id = self.next_id;
        self.next_id += 1;
        let response = self
            .request(Method::POST)
            .json(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .send()
            .await
            .map_err(|_| "Zhipu Search MCP request failed".to_string())?;
        check_status(&response)?;
        if method == "initialize" {
            self.session_id = response.headers().get("Mcp-Session-Id").cloned();
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or("");
        if mime == "application/json" {
            let value = response
                .json()
                .await
                .map_err(|_| "Invalid Zhipu Search MCP JSON response".to_string())?;
            return decode_reply(value, id)?.ok_or("Zhipu Search MCP response id mismatch".into());
        }
        if mime != "text/event-stream" {
            return Err("Unsupported Zhipu Search MCP response content type".into());
        }
        let mut decoder = SseDecoder::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "Zhipu Search MCP stream read failed".to_string())?;
            for event in decoder.decode(&chunk) {
                let value = serde_json::from_str(&event.data)
                    .map_err(|_| "Invalid Zhipu Search MCP SSE JSON".to_string())?;
                if let Some(reply) = decode_reply(value, id)? {
                    return Ok(reply);
                }
            }
        }
        Err("Zhipu Search MCP stream ended without a response".into())
    }
}

// 正常完成、出错、超时与 future 被取消时均终止服务端会话。
impl Drop for Session {
    fn drop(&mut self) {
        if self.session_id.is_some() {
            let request = self
                .request(Method::DELETE)
                .timeout(Duration::from_millis(1500));
            tokio::spawn(async move {
                let _ = request.send().await;
            });
        }
    }
}

fn check_status(response: &Response) -> Result<(), String> {
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("Zhipu Search MCP request failed (status {status})"))
    }
}

#[derive(Deserialize)]
struct RpcMessage {
    jsonrpc: String,
    id: Option<Value>,
    result: Option<Value>,
    error: Option<RpcError>,
    method: Option<String>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
}

fn decode_reply<T: DeserializeOwned>(value: Value, id: u64) -> Result<Option<T>, String> {
    let message: RpcMessage = serde_json::from_value(value)
        .map_err(|_| "Invalid Zhipu Search MCP JSON-RPC message".to_string())?;
    if message.jsonrpc != "2.0" {
        return Err("Invalid Zhipu Search MCP JSON-RPC version".into());
    }
    if message.id.is_none() && message.method.is_some() {
        return Ok(None);
    }
    if message.id != Some(json!(id)) {
        return Err("Zhipu Search MCP response id mismatch".into());
    }
    if let Some(error) = message.error {
        return Err(format!(
            "Zhipu Search MCP JSON-RPC error (code {})",
            error.code
        ));
    }
    let result = message
        .result
        .ok_or("Zhipu Search MCP response is missing result")?;
    serde_json::from_value(result)
        .map(Some)
        .map_err(|_| "Invalid Zhipu Search MCP result".to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult {
    protocol_version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolList {
    tools: Vec<SearchTool>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct SearchTool {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolResult {
    content: Vec<Content>,
    #[serde(default)]
    is_error: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

pub(super) async fn search(
    endpoint: &str,
    api_key: &str,
    query: &str,
    max_results: usize,
) -> Result<Vec<WebSearchResult>, String> {
    let mut session = Session {
        endpoint: endpoint.into(),
        api_key: api_key.into(),
        session_id: None,
        protocol_version: None,
        next_id: 1,
    };
    let init: InitializeResult = session
        .call(
            "initialize",
            json!({
                "protocolVersion":"2025-06-18", "capabilities":{},
                "clientInfo":{"name":"vva", "version":env!("CARGO_PKG_VERSION")}
            }),
        )
        .await?;
    if !["2024-11-05", "2025-03-26", "2025-06-18"].contains(&init.protocol_version.as_str()) {
        return Err("Unsupported Zhipu Search MCP protocol version".into());
    }
    session.protocol_version = Some(init.protocol_version);
    let response = session
        .request(Method::POST)
        .json(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
        .send()
        .await
        .map_err(|_| "Zhipu Search MCP initialization notification failed".to_string())?;
    check_status(&response)?;

    let mut params = json!({});
    let mut cursors = HashSet::new();
    let name = loop {
        let page: ToolList = session.call("tools/list", params).await?;
        // 仅调用服务端实际声明的搜索工具名，与参考项目支持的两种部署一致。
        if let Some(tool) = page
            .tools
            .into_iter()
            .find(|tool| matches!(tool.name.as_str(), "webSearchPrime" | "web_search_prime"))
        {
            break tool.name;
        }
        let cursor = page
            .next_cursor
            .ok_or("Zhipu Search MCP search tool not found")?;
        if !cursors.insert(cursor.clone()) {
            return Err("Zhipu Search MCP returned a repeated tool cursor".into());
        }
        params = json!({"cursor":cursor});
    };
    let result: ToolResult = session
        .call(
            "tools/call",
            json!({
                "name":name, "arguments":{"search_query":query}
            }),
        )
        .await?;
    parse_results(result, max_results)
}

fn parse_results(result: ToolResult, max_results: usize) -> Result<Vec<WebSearchResult>, String> {
    if result.is_error {
        return Err("Zhipu Search MCP search tool returned an error".into());
    }
    let mut hits = Vec::new();
    let mut has_text = false;
    for content in result.content {
        if let Content::Text { text } = content {
            has_text = true;
            let mut value: Value = serde_json::from_str(&text)
                .map_err(|_| "Invalid Zhipu Search MCP search JSON".to_string())?;
            // 参考服务的 text 内容可为 JSON 数组或经 JSON 编码的数组字符串。
            if let Value::String(encoded) = value {
                value = serde_json::from_str(&encoded)
                    .map_err(|_| "Invalid Zhipu Search MCP encoded search JSON".to_string())?;
            }
            let items: Vec<SearchHit> = serde_json::from_value(value)
                .map_err(|_| "Invalid Zhipu Search MCP search result list".to_string())?;
            hits.extend(items);
        }
    }
    if !has_text {
        return Err("Zhipu Search MCP returned no search text".into());
    }
    Ok(project(hits, max_results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_search_text_and_rpc_envelopes() {
        for text in ["[]", "\"[]\""] {
            let result = ToolResult {
                content: vec![Content::Text { text: text.into() }],
                is_error: false,
            };
            assert!(parse_results(result, 1).unwrap().is_empty());
        }
        for text in [
            "invalid",
            "{}",
            "\"{}\"",
            r#"[{"url":"https://example.com"}]"#,
        ] {
            let result = ToolResult {
                content: vec![Content::Text { text: text.into() }],
                is_error: false,
            };
            assert!(parse_results(result, 1).is_err());
        }
        assert!(
            parse_results(
                ToolResult {
                    content: vec![],
                    is_error: false
                },
                1
            )
            .is_err()
        );
        for envelope in [
            json!({"jsonrpc":"1.0","id":1,"result":{}}),
            json!({"jsonrpc":"2.0","id":2,"result":{}}),
            json!({"jsonrpc":"2.0","id":1}),
        ] {
            assert!(decode_reply::<Value>(envelope, 1).is_err());
        }
        assert!(
            decode_reply::<Value>(
                json!({"jsonrpc":"2.0","method":"notifications/message","params":{}}),
                1
            )
            .unwrap()
            .is_none()
        );
    }
}
