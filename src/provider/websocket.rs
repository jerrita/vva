//! JSON WebSocket 传输。独立读取任务在工具执行及空闲期间继续处理控制帧。

use std::sync::OnceLock;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use reqwest::header::{
    CONNECTION, HeaderMap, HeaderValue, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_EXTENSIONS,
    SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL, SEC_WEBSOCKET_VERSION, UPGRADE,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::{client::generate_key, derive_accept_key},
    protocol::Role,
};

use super::{ProviderError, client_builder, http::add_app_attribution};

// RFC 6455 的 Upgrade 使用 HTTP/1.1；仅设置单个请求的 version 不会禁用 TLS ALPN 的 h2。
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

type Events = mpsc::Sender<Result<Value, ProviderError>>;

struct Request {
    text: String,
    events: Events,
}

pub(super) struct JsonWebSocket {
    requests: mpsc::Sender<Request>,
}

impl JsonWebSocket {
    pub(super) async fn connect(
        url: &str,
        mut headers: HeaderMap,
        terminal: fn(&Value) -> bool,
    ) -> Result<Self, ProviderError> {
        // 独立的 HTTP/1.1 连接池沿用公共客户端配置，保留代理、证书与超时行为。
        add_app_attribution(url, &mut headers);
        let key = generate_key();
        headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(SEC_WEBSOCKET_VERSION, HeaderValue::from_static("13"));
        headers.insert(
            SEC_WEBSOCKET_KEY,
            HeaderValue::from_str(&key).expect("generated websocket key is ASCII"),
        );
        let response = CLIENT
            .get_or_init(|| {
                client_builder()
                    .http1_only()
                    .build()
                    .expect("failed to build WebSocket HTTP client")
            })
            .get(url)
            .version(reqwest::Version::HTTP_11)
            .headers(headers)
            .send()
            .await?;
        let status = response.status();
        if status != reqwest::StatusCode::SWITCHING_PROTOCOLS {
            return Err(ProviderError::ApiError {
                status,
                body: response.text().await?,
            });
        }
        let token = |name, expected: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(',')
                        .any(|part| part.trim().eq_ignore_ascii_case(expected))
                })
        };
        let accept = derive_accept_key(key.as_bytes());
        if !token(CONNECTION, "upgrade")
            || !token(UPGRADE, "websocket")
            || response
                .headers()
                .get(SEC_WEBSOCKET_ACCEPT)
                .map(|v| v.as_bytes())
                != Some(accept.as_bytes())
            || response.headers().contains_key(SEC_WEBSOCKET_EXTENSIONS)
            || response.headers().contains_key(SEC_WEBSOCKET_PROTOCOL)
        {
            return Err(ProviderError::Protocol(
                "Invalid WebSocket upgrade response".into(),
            ));
        }
        let socket =
            WebSocketStream::from_raw_socket(response.upgrade().await?, Role::Client, None).await;
        let (requests, receiver) = mpsc::channel(1);
        tokio::spawn(run(socket, receiver, terminal));
        Ok(Self { requests })
    }

    pub(super) fn is_closed(&self) -> bool {
        self.requests.is_closed()
    }

    pub(super) async fn request(
        &self,
        text: String,
    ) -> Result<mpsc::Receiver<Result<Value, ProviderError>>, ProviderError> {
        let (events, receiver) = mpsc::channel(64);
        self.requests
            .send(Request { text, events })
            .await
            .map_err(|_| {
                ProviderError::Protocol("WebSocket connection closed before request".into())
            })?;
        Ok(receiver)
    }
}

async fn run(
    mut socket: WebSocketStream<reqwest::Upgraded>,
    mut requests: mpsc::Receiver<Request>,
    terminal: fn(&Value) -> bool,
) {
    let mut active: Option<Events> = None;
    loop {
        tokio::select! {
            // 丢弃调用 future 会关闭结果接收端；立即关闭连接，丢弃在途响应。
            _ = async {
                match &active {
                    Some(events) => events.closed().await,
                    None => std::future::pending().await,
                }
            } => break,
            request = requests.recv() => {
                let Some(request) = request else { break };
                if active.is_some() {
                    let _ = request.events.send(Err(ProviderError::Protocol(
                        "Concurrent requests on a session WebSocket".into(),
                    ))).await;
                    break;
                }
                let sent = tokio::select! {
                    _ = request.events.closed() => break,
                    result = tokio::time::timeout(
                        Duration::from_secs(30),
                        socket.send(Message::Text(request.text.into())),
                    ) => match result {
                        Ok(result) => result.map_err(ProviderError::from),
                        Err(_) => Err(ProviderError::Protocol("WebSocket write timed out".into())),
                    },
                };
                if let Err(error) = sent {
                    let _ = request.events.send(Err(error)).await;
                    break;
                }
                active = Some(request.events);
            }
            message = socket.next() => {
                let value = match message {
                    Some(Ok(Message::Text(text))) => serde_json::from_str::<Value>(&text)
                        .map_err(|error| ProviderError::Protocol(format!("Invalid WebSocket JSON: {error}"))),
                    Some(Ok(Message::Ping(_))) => {
                        // tungstenite 已排入 Pong；即使没有新请求也要冲刷。
                        if socket.flush().await.is_err() { break; }
                        continue;
                    }
                    Some(Ok(Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => Err(ProviderError::Protocol(
                        "Expected a WebSocket JSON text frame".into(),
                    )),
                    Some(Err(error)) => Err(error.into()),
                };
                let Some(events) = active.as_ref() else { break };
                let finished = value.as_ref().is_ok_and(terminal);
                let failed = value.is_err();
                if events.send(value).await.is_err() || failed { break; }
                if finished { active = None; }
            }
        }
    }
}
