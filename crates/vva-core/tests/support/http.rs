//! 协议测试用 HTTP/1.1 服务端，避免核心测试依赖 Web 应用框架。
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::{future::Future, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Value,
}

pub(crate) struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
    pub keep_open: bool,
}

impl Response {
    pub fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type", "application/json".into())],
            body: value.to_string(),
            keep_open: false,
        }
    }
    pub fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            keep_open: false,
        }
    }
}

pub(crate) async fn serve<F, Fut>(handler: F) -> (String, tokio::task::JoinHandle<()>)
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let handler = handler.clone();
                    connections.spawn(async move {
                        let mut bytes = Vec::new();
                        let mut buf = [0; 4096];
                        let header_end = loop {
                            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") { break end + 4; }
                            let n = socket.read(&mut buf).await.unwrap();
                            assert!(n > 0, "request ended before headers");
                            bytes.extend_from_slice(&buf[..n]);
                        };
                        let head = std::str::from_utf8(&bytes[..header_end]).unwrap();
                        let mut lines = head.lines();
                        let mut first = lines.next().unwrap().split_whitespace();
                        let method = first.next().unwrap().to_string();
                        let path = first.next().unwrap().to_string();
                        let mut headers = HeaderMap::new();
                        for line in lines.filter(|line| !line.is_empty()) {
                            let (name, value) = line.split_once(':').unwrap();
                            headers.insert(HeaderName::from_bytes(name.as_bytes()).unwrap(), HeaderValue::from_str(value.trim()).unwrap());
                        }
                        let length = headers.get("content-length").map_or(0, |value| value.to_str().unwrap().parse::<usize>().unwrap());
                        while bytes.len() < header_end + length {
                            let n = socket.read(&mut buf).await.unwrap();
                            assert!(n > 0, "request ended before body");
                            bytes.extend_from_slice(&buf[..n]);
                        }
                        let body = if length == 0 { Value::Null } else { serde_json::from_slice(&bytes[header_end..header_end+length]).unwrap() };
                        let response = handler(Request { method, path, headers, body }).await;
                        let mut head = format!("HTTP/1.1 {} Fixture\r\n", response.status);
                        for (name, value) in response.headers { head.push_str(&format!("{name}: {value}\r\n")); }
                        if response.keep_open {
                            head.push_str("transfer-encoding: chunked\r\n\r\n");
                            socket.write_all(head.as_bytes()).await.unwrap();
                            socket.write_all(format!("{:x}\r\n{}\r\n", response.body.len(), response.body).as_bytes()).await.unwrap();
                            // 不发送结束块：客户端必须在匹配的 MCP 响应到达后立即返回。
                            let _ = socket.read(&mut buf).await;
                        } else {
                            head.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n{}", response.body.len(), response.body));
                            socket.write_all(head.as_bytes()).await.unwrap();
                        }
                    });
                }
                Some(result) = connections.join_next() => { result.unwrap(); }
            }
        }
    });
    (endpoint, server)
}
