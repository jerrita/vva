//! QQ 网关 WebSocket 客户端：identify / resume / heartbeat / 断线重连。
//!
//! 协议状态机（[`Session`]）与连接 IO（[`Gateway`]）分离，前者可纯函数测试。
//! resume 时服务端只重放 `seq` 之后的事件，网关仅在事件成功转发后才继续读取下一帧，故不会重复投递。

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::{Instant, Interval, interval_at};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use super::api::QqApi;
use super::event::{MessageEvent, parse_message_event};
use crate::space::channel::{ChannelError, Result};

const OP_DISPATCH: u8 = 0;
const OP_HEARTBEAT: u8 = 1;
const OP_IDENTIFY: u8 = 2;
const OP_RESUME: u8 = 6;
const OP_RECONNECT: u8 = 7;
const OP_INVALID_SESSION: u8 = 9;
const OP_HELLO: u8 = 10;
const OP_HEARTBEAT_ACK: u8 = 11;

/// PUBLIC_GUILD_MESSAGES (1<<30) | DIRECT_MESSAGE (1<<12) | GROUP_AND_C2C_EVENT (1<<25)。
const INTENTS: u64 = (1 << 30) | (1 << 12) | (1 << 25);

/// 关闭码（官方 WebSocket 错误码表）：4008 发送过快，可 resume。
const RATE_LIMITED: u16 = 4008;
/// 4009 连接过期，可 resume。
const SESSION_EXPIRED: u16 = 4009;
const BACKOFF_SECS: [u64; 6] = [1, 2, 5, 10, 30, 60];

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Reconnect {
    reason: String,
    /// 覆盖默认退避时长。
    delay: Option<Duration>,
    /// 丢弃 session_id / seq 并刷新 token（下次重新 identify）。
    reset: bool,
}

impl Reconnect {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            delay: None,
            reset: false,
        }
    }
}

#[derive(Debug)]
enum Step {
    Hello(Duration),
    Ready,
    Resumed,
    Ack,
    Message(MessageEvent),
    Reconnect(Reconnect),
    Ignore,
}

enum Exit {
    Stop,
    Reconnect(Reconnect),
    /// 官方标注既不可 resume 也不可 identify 的关闭码。
    Fatal(ChannelError),
}

#[derive(Deserialize)]
struct Payload {
    op: u8,
    s: Option<u64>,
    t: Option<String>,
    d: Option<Value>,
}

#[derive(Deserialize)]
struct Hello {
    heartbeat_interval: u64,
}

#[derive(Deserialize)]
struct Ready {
    session_id: String,
}

/// 网关会话状态：resume 所需的 session_id 与最近 seq。
#[derive(Default)]
struct Session {
    id: Option<String>,
    seq: Option<u64>,
}

impl Session {
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// 收到 Hello 后的首帧：可续传则 resume，否则 identify。
    fn handshake(&self, access_token: &str) -> Value {
        let token = format!("QQBot {access_token}");
        match (&self.id, self.seq) {
            (Some(id), Some(seq)) => json!({
                "op": OP_RESUME,
                "d": { "token": token, "session_id": id, "seq": seq },
            }),
            _ => json!({
                "op": OP_IDENTIFY,
                "d": { "token": token, "intents": INTENTS, "shard": [0, 1] },
            }),
        }
    }

    fn heartbeat(&self) -> Value {
        json!({ "op": OP_HEARTBEAT, "d": self.seq })
    }

    fn on_payload(&mut self, text: &str) -> Result<Step> {
        let p: Payload = serde_json::from_str(text)
            .map_err(|e| ChannelError::Protocol(format!("网关帧非法: {e}")))?;
        if let Some(s) = p.s {
            self.seq = Some(s);
        }
        match p.op {
            OP_HELLO => {
                let hello: Hello = from_data(p.d, "HELLO")?;
                if hello.heartbeat_interval == 0 {
                    return Err(ChannelError::Protocol("HELLO 心跳间隔为 0".into()));
                }
                Ok(Step::Hello(Duration::from_millis(hello.heartbeat_interval)))
            }
            OP_DISPATCH => {
                let event =
                    p.t.ok_or_else(|| ChannelError::Protocol("DISPATCH 缺少 t".into()))?;
                match event.as_str() {
                    "READY" => {
                        self.id = Some(from_data::<Ready>(p.d, "READY")?.session_id);
                        Ok(Step::Ready)
                    }
                    "RESUMED" => Ok(Step::Resumed),
                    _ => Ok(parse_message_event(&event, p.d.unwrap_or(Value::Null))?
                        .map_or(Step::Ignore, Step::Message)),
                }
            }
            OP_RECONNECT => Ok(Step::Reconnect(Reconnect::new("服务端要求重连"))),
            // identify / resume 参数有误，官方未定义载荷：一律重新 identify。
            OP_INVALID_SESSION => Ok(Step::Reconnect(Reconnect {
                reason: "session 无效".into(),
                delay: Some(Duration::from_secs(3)),
                reset: true,
            })),
            OP_HEARTBEAT_ACK => Ok(Step::Ack),
            _ => Ok(Step::Ignore),
        }
    }
}

fn from_data<T: serde::de::DeserializeOwned>(d: Option<Value>, what: &str) -> Result<T> {
    serde_json::from_value(d.unwrap_or(Value::Null))
        .map_err(|e| ChannelError::Protocol(format!("{what} 载荷非法: {e}")))
}

/// 按官方关闭码表处置：4008 / 4009 resume；不可重连的码终止网关；
/// 其余 QQ 码（≥4000）重新 identify；标准 WebSocket 关闭码按网络中断处理（resume）。
fn close_exit(frame: Option<CloseFrame>) -> Exit {
    let Some(frame) = frame else {
        return Exit::Reconnect(Reconnect::new("连接关闭（无关闭帧）"));
    };
    let code = u16::from(frame.code);
    let reason = format!("连接关闭 code={code} reason={}", frame.reason);
    match code {
        4001 | 4002 | 4010..=4014 | 4914 | 4915 => {
            Exit::Fatal(ChannelError::Ws(format!("{reason}（不可重连）")))
        }
        _ => Exit::Reconnect(Reconnect {
            reason,
            delay: (code == RATE_LIMITED).then_some(Duration::from_secs(60)),
            reset: code >= 4000 && !matches!(code, RATE_LIMITED | SESSION_EXPIRED),
        }),
    }
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_secs(BACKOFF_SECS[attempt.min(BACKOFF_SECS.len() - 1)])
}

fn ws_err(e: tokio_tungstenite::tungstenite::Error) -> ChannelError {
    ChannelError::Ws(e.to_string())
}

pub(super) struct Gateway {
    api: Arc<QqApi>,
    events: mpsc::Sender<MessageEvent>,
    session: Session,
}

impl Gateway {
    pub(super) fn new(api: Arc<QqApi>, events: mpsc::Sender<MessageEvent>) -> Self {
        Self {
            api,
            events,
            session: Session::default(),
        }
    }

    /// 持续连接并转发消息事件；事件接收端关闭时正常返回，遇不可重连的关闭码返回错误。
    pub(super) async fn run(mut self) -> Result<()> {
        let mut attempt = 0;
        loop {
            let r = match self.connect_once(&mut attempt).await {
                Ok(Exit::Stop) => return Ok(()),
                Ok(Exit::Fatal(e)) => return Err(e),
                Ok(Exit::Reconnect(r)) => r,
                Err(e) => Reconnect::new(e.to_string()),
            };
            if r.reset {
                self.session.reset();
                self.api.invalidate_token().await;
            }
            let delay = r.delay.unwrap_or_else(|| backoff(attempt));
            attempt += 1;
            tracing::warn!("QQ 网关 {:.0}s 后重连: {}", delay.as_secs_f32(), r.reason);
            tokio::time::sleep(delay).await;
        }
    }

    /// 单次连接的收发循环；`attempt` 在握手成功（READY / RESUMED）后清零。
    async fn connect_once(&mut self, attempt: &mut usize) -> Result<Exit> {
        let url = self.api.gateway_url().await?;
        let token = self.api.access_token().await?;
        tracing::info!("QQ 网关连接 {url}");
        let (ws, _) = connect_async(url.as_str()).await.map_err(ws_err)?;
        let (mut sink, mut stream) = ws.split();

        let mut heartbeat: Option<Interval> = None;
        let mut awaiting_ack = false;
        loop {
            tokio::select! {
                frame = stream.next() => {
                    let Some(frame) = frame else {
                        return Ok(Exit::Reconnect(Reconnect::new("连接被对端断开")));
                    };
                    let text = match frame.map_err(ws_err)? {
                        Message::Text(t) => t,
                        Message::Close(frame) => return Ok(close_exit(frame)),
                        _ => continue,
                    };
                    match self.session.on_payload(text.as_str()) {
                        Ok(Step::Hello(period)) => {
                            heartbeat = Some(interval_at(Instant::now() + period, period));
                            let hs = self.session.handshake(&token).to_string();
                            sink.send(Message::text(hs)).await.map_err(ws_err)?;
                        }
                        Ok(Step::Ready) => {
                            tracing::info!("QQ 网关 READY");
                            *attempt = 0;
                        }
                        Ok(Step::Resumed) => {
                            tracing::info!("QQ 网关 RESUMED");
                            *attempt = 0;
                        }
                        Ok(Step::Ack) => awaiting_ack = false,
                        Ok(Step::Message(ev)) => {
                            if self.events.send(ev).await.is_err() {
                                return Ok(Exit::Stop);
                            }
                        }
                        Ok(Step::Reconnect(r)) => return Ok(Exit::Reconnect(r)),
                        Ok(Step::Ignore) => {}
                        Err(e) => tracing::warn!("QQ 网关帧被忽略: {e}"),
                    }
                }
                _ = async {
                    match heartbeat.as_mut() {
                        Some(i) => { i.tick().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    if awaiting_ack {
                        return Ok(Exit::Reconnect(Reconnect::new("心跳无 ACK")));
                    }
                    awaiting_ack = true;
                    let hb = self.session.heartbeat().to_string();
                    sink.send(Message::text(hb)).await.map_err(ws_err)?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    use super::*;

    #[test]
    fn hello_then_identify() {
        let mut s = Session::default();
        let step = s
            .on_payload(r#"{"op":10,"d":{"heartbeat_interval":41250}}"#)
            .unwrap();
        assert!(matches!(step, Step::Hello(d) if d == Duration::from_millis(41250)));
        let hs = s.handshake("TOK");
        assert_eq!(hs["op"], 2);
        assert_eq!(hs["d"]["token"], "QQBot TOK");
        assert_eq!(hs["d"]["intents"], (1u64 << 30) | (1 << 12) | (1 << 25));
        assert_eq!(hs["d"]["shard"], json!([0, 1]));
        assert_eq!(s.heartbeat(), json!({"op":1,"d":null}));
    }

    #[test]
    fn ready_then_resume_with_last_seq() {
        let mut s = Session::default();
        let step = s
            .on_payload(r#"{"op":0,"s":1,"t":"READY","d":{"version":1,"session_id":"SID","user":{"id":"b"},"shard":[0,1]}}"#)
            .unwrap();
        assert!(matches!(step, Step::Ready));
        s.on_payload(r#"{"op":0,"s":7,"t":"GUILD_CREATE","d":{}}"#)
            .unwrap();
        let hs = s.handshake("T2");
        assert_eq!(hs["op"], 6);
        assert_eq!(
            hs["d"],
            json!({"token":"QQBot T2","session_id":"SID","seq":7})
        );
        assert_eq!(s.heartbeat(), json!({"op":1,"d":7}));
    }

    #[test]
    fn invalid_session_and_reconnect() {
        let mut s = Session::default();
        let Step::Reconnect(r) = s.on_payload(r#"{"op":9}"#).unwrap() else {
            panic!()
        };
        assert!(r.reset);
        assert_eq!(r.delay, Some(Duration::from_secs(3)));
        let Step::Reconnect(r) = s.on_payload(r#"{"op":7}"#).unwrap() else {
            panic!()
        };
        assert!(!r.reset && r.delay.is_none());
    }

    #[test]
    fn close_code_policy() {
        let close = |code: u16| {
            close_exit(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: "".into(),
            }))
        };
        let reconnect = |code| match close(code) {
            Exit::Reconnect(r) => r,
            _ => panic!("{code} 应重连"),
        };
        let r = reconnect(4008);
        assert_eq!(r.delay, Some(Duration::from_secs(60)));
        assert!(!r.reset);
        assert!(!reconnect(4009).reset);
        for c in [4004, 4006, 4007, 4900, 4913] {
            assert!(reconnect(c).reset, "{c}");
        }
        assert!(!reconnect(1006).reset);
        for c in [4001, 4002, 4010, 4014, 4914, 4915] {
            assert!(matches!(close(c), Exit::Fatal(_)), "{c}");
        }
        assert!(matches!(close_exit(None), Exit::Reconnect(r) if !r.reset));
    }
}
