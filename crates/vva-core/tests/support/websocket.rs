use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as Frame;

struct CheckHandshake;

impl tokio_tungstenite::tungstenite::handshake::server::Callback for CheckHandshake {
    fn on_request(
        self,
        request: &tokio_tungstenite::tungstenite::handshake::server::Request,
        response: tokio_tungstenite::tungstenite::handshake::server::Response,
    ) -> Result<
        tokio_tungstenite::tungstenite::handshake::server::Response,
        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
    > {
        assert_eq!(request.uri().path(), "/v1/responses");
        assert!(request.headers().contains_key("authorization"));
        Ok(response)
    }
}

pub(crate) struct CapturedRequest {
    pub connection: usize,
    pub body: Value,
    frames: mpsc::Sender<Frame>,
}

impl CapturedRequest {
    pub(crate) async fn send(&self, frame: Frame) {
        self.frames.send(frame).await.unwrap();
    }

    pub(crate) async fn reply(&self, event: Value) {
        self.send(Frame::Text(event.to_string().into())).await;
    }
}

pub(crate) struct MockWebSocket {
    pub url: String,
    pub(crate) requests: mpsc::Receiver<CapturedRequest>,
    pub(crate) pongs: mpsc::Receiver<()>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for MockWebSocket {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl MockWebSocket {
    pub(crate) async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (requests, receiver) = mpsc::channel(16);
        let (pongs, pong_rx) = mpsc::channel(16);
        let server = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            let mut connection = 0;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        connection += 1;
                        let requests = requests.clone();
                        let pongs = pongs.clone();
                        connections.spawn(async move {
                            let mut socket = tokio_tungstenite::accept_hdr_async(stream, CheckHandshake).await.unwrap();
                            let (frames, mut responses) = mpsc::channel(16);
                            loop {
                                tokio::select! {
                                    frame = responses.recv() => {
                                        let Some(frame) = frame else { break };
                                        if socket.send(frame).await.is_err() { break; }
                                    }
                                    frame = socket.next() => match frame {
                                        Some(Ok(Frame::Text(text))) => {
                                            let body = serde_json::from_str(&text).unwrap();
                                            if requests.send(CapturedRequest { connection, body, frames: frames.clone() }).await.is_err() { break; }
                                        }
                                        Some(Ok(Frame::Pong(_))) => { let _ = pongs.send(()).await; }
                                        _ => break,
                                    }
                                }
                            }
                        });
                    }
                    Some(result) = connections.join_next() => { result.unwrap(); }
                }
            }
        });
        Self {
            url,
            requests: receiver,
            pongs: pong_rx,
            server,
        }
    }

    pub(crate) async fn next(&mut self) -> CapturedRequest {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .expect("mock request timed out")
            .unwrap()
    }
}

pub(crate) fn completed(id: &str, output: Vec<Value>) -> Value {
    json!({"type": "response.completed", "sequence_number": 4, "response": {
        "id": id, "object": "response", "created_at": 0, "model": "m",
        "status": "completed", "store": false, "output": output, "usage": null
    }})
}

pub(crate) fn answer(id: &str) -> Value {
    completed(
        id,
        vec![json!({"type": "message", "id": format!("msg_{id}"),
        "role": "assistant", "status": "completed", "content": [
            {"type": "output_text", "text": "fixture answer", "annotations": []}
        ]})],
    )
}
