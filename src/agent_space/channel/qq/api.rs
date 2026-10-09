//! QQ 机器人 OpenAPI 客户端：access token 刷新、消息发送、富媒体上传。

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::text::{MAX_MESSAGE_CHARS, PASSIVE_REPLY_LIMIT, chunk_text, strip_markdown};
use crate::agent_space::channel::{ChannelError, Result};

const API_BASE: &str = "https://api.sgroup.qq.com";
const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// QQ 聊天类型，对应 binding 的第二段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    Group,
    C2c,
    Guild,
    Dm,
}

impl MsgType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::C2c => "c2c",
            Self::Guild => "guild",
            Self::Dm => "dm",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [Self::Group, Self::C2c, Self::Guild, Self::Dm]
            .into_iter()
            .find(|t| t.as_str() == s)
    }

    /// 群聊 / 单聊走 v2 消息接口（带 msg_type / msg_seq），频道与频道私信走旧接口。
    fn is_v2(self) -> bool {
        matches!(self, Self::Group | Self::C2c)
    }
}

/// 消息目标：聊天类型 + chat id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub msg_type: MsgType,
    pub chat_id: String,
}

impl Target {
    /// `qq:<msg_type>:<chat_id>`。
    pub fn binding(&self) -> String {
        format!("qq:{}:{}", self.msg_type.as_str(), self.chat_id)
    }

    pub fn from_binding(binding: &str) -> Result<Self> {
        let invalid = || ChannelError::Protocol(format!("非法 QQ binding: {binding}"));
        let rest = binding.strip_prefix("qq:").ok_or_else(invalid)?;
        let (ty, chat_id) = rest.split_once(':').ok_or_else(invalid)?;
        let msg_type = MsgType::parse(ty).ok_or_else(invalid)?;
        if chat_id.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            msg_type,
            chat_id: chat_id.to_string(),
        })
    }

    fn messages_path(&self) -> String {
        let id = &self.chat_id;
        match self.msg_type {
            MsgType::Group => format!("/v2/groups/{id}/messages"),
            MsgType::C2c => format!("/v2/users/{id}/messages"),
            MsgType::Guild => format!("/channels/{id}/messages"),
            MsgType::Dm => format!("/dms/{id}/messages"),
        }
    }

    /// 富媒体上传仅 group / c2c 支持，其余显式报错。
    pub fn upload_path(&self) -> Result<String> {
        let id = &self.chat_id;
        match self.msg_type {
            MsgType::Group => Ok(format!("/v2/groups/{id}/files")),
            MsgType::C2c => Ok(format!("/v2/users/{id}/files")),
            other => Err(ChannelError::Unsupported(format!(
                "QQ {} 聊天不支持发送本地文件（仅 group/c2c 支持富媒体上传）",
                other.as_str()
            ))),
        }
    }
}

struct Token {
    value: String,
    expires_at: Instant,
}

impl Token {
    /// 剩余寿命不足 min(300s, max(15s, 剩余/3)) 时视为需要刷新。
    fn is_fresh(&self) -> bool {
        let remaining = self.expires_at.saturating_duration_since(Instant::now());
        let ahead = (remaining / 3).clamp(Duration::from_secs(15), Duration::from_secs(300));
        remaining > ahead
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    /// 官方文档中为字符串（如 "7200"）。
    expires_in: String,
}

#[derive(Deserialize)]
struct GatewayResponse {
    url: String,
}

#[derive(Deserialize)]
struct SentMessage {
    id: String,
    ext_info: Option<ExtInfo>,
}

#[derive(Deserialize)]
struct ExtInfo {
    ref_idx: Option<String>,
}

impl SentMessage {
    /// 用户后续引用该消息时入站携带的 id：v2 接口为 `ext_info.ref_idx`，其余为消息 id。
    fn quote_id(self) -> String {
        self.ext_info.and_then(|e| e.ref_idx).unwrap_or(self.id)
    }
}

pub struct QqApi {
    http: reqwest::Client,
    app_id: String,
    secret: String,
    base_url: String,
    token_url: String,
    token: Mutex<Option<Token>>,
    /// msg_seq 只需对同一 msg_id 唯一，进程级自增即可。
    seq: AtomicU32,
}

impl QqApi {
    pub fn new(app_id: String, secret: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            app_id,
            secret,
            base_url: API_BASE.into(),
            token_url: TOKEN_URL.into(),
            token: Mutex::new(None),
            seq: AtomicU32::new(1),
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub async fn access_token(&self) -> Result<String> {
        let mut guard = self.token.lock().await;
        if let Some(t) = guard.as_ref().filter(|t| t.is_fresh()) {
            return Ok(t.value.clone());
        }
        let resp = self
            .http
            .post(&self.token_url)
            .timeout(REQUEST_TIMEOUT)
            .json(&json!({ "appId": self.app_id, "clientSecret": self.secret }))
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &self.token_url, &body));
        }
        let parsed: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| ChannelError::Protocol(format!("token 响应非法: {e}")))?;
        let secs: u64 = parsed.expires_in.parse().map_err(|_| {
            ChannelError::Protocol(format!("expires_in 非法: {}", parsed.expires_in))
        })?;
        let value = parsed.access_token;
        *guard = Some(Token {
            value: value.clone(),
            expires_at: Instant::now() + Duration::from_secs(secs.max(60)),
        });
        Ok(value)
    }

    pub async fn invalidate_token(&self) {
        *self.token.lock().await = None;
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<T> {
        let mut token = self.access_token().await?;
        let mut retried = false;
        loop {
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.base_url))
                .header("Authorization", format!("QQBot {token}"))
                .header("X-Union-Appid", &self.app_id)
                .timeout(timeout);
            if let Some(body) = body {
                req = req.json(body);
            }
            let resp = req.send().await?;
            let status = resp.status();
            if status.as_u16() == 401 && !retried {
                retried = true;
                tracing::warn!("QQ 请求 401，刷新 access token 重试一次: {path}");
                self.invalidate_token().await;
                token = self.access_token().await?;
                continue;
            }
            let text = resp.text().await?;
            if !status.is_success() {
                return Err(api_error(status.as_u16(), path, &text));
            }
            return serde_json::from_str(&text)
                .map_err(|e| ChannelError::Protocol(format!("{path} 响应非法: {e}")));
        }
    }

    pub async fn gateway_url(&self) -> Result<String> {
        let resp: GatewayResponse = self
            .request(Method::GET, "/gateway", None, REQUEST_TIMEOUT)
            .await?;
        Ok(resp.url)
    }

    async fn post_message(&self, target: &Target, body: Value) -> Result<String> {
        let sent: SentMessage = self
            .request(
                Method::POST,
                &target.messages_path(),
                Some(&body),
                REQUEST_TIMEOUT,
            )
            .await?;
        Ok(sent.quote_id())
    }

    /// 分段发送 Markdown 文本；Markdown 被拒（非认证 / 限流 / 服务端错误）时该段降级纯文本。
    ///
    /// 被动回复（`reply_to` 有值）受条数上限约束，超出部分在末段明确标注截断。
    pub async fn send_text(
        &self,
        target: &Target,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<Vec<String>> {
        let limit = reply_to.map(|_| PASSIVE_REPLY_LIMIT);
        let mut ids = Vec::new();
        for chunk in chunk_text(text, MAX_MESSAGE_CHARS, limit) {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);
            let markdown = message_payload(target, &chunk, true, reply_to, seq);
            let id = match self.post_message(target, markdown).await {
                Err(ChannelError::Api { status, .. }) if markdown_degradable(status) => {
                    tracing::warn!("QQ markdown 发送失败（{status}），降级纯文本");
                    let plain =
                        message_payload(target, &strip_markdown(&chunk), false, reply_to, seq);
                    self.post_message(target, plain).await?
                }
                other => other?,
            };
            ids.push(id);
        }
        Ok(ids)
    }

    /// 上传 base64 文件（`file_type`：1 图片 / 2 视频 / 3 语音 / 4 文件）并以 msg_type=7 发出。
    pub async fn send_media(
        &self,
        target: &Target,
        file_type: u8,
        file_name: &str,
        file_data: String,
        reply_to: Option<&str>,
    ) -> Result<String> {
        let upload = json!({
            "file_type": file_type,
            "file_data": file_data,
            "file_name": file_name,
            "srv_send_msg": false,
        });
        let media: Value = self
            .request(
                Method::POST,
                &target.upload_path()?,
                Some(&upload),
                UPLOAD_TIMEOUT,
            )
            .await?;
        if media.as_object().is_none_or(|o| o.is_empty()) {
            return Err(ChannelError::Protocol(format!(
                "富媒体上传返回为空: {file_name}"
            )));
        }
        let mut body = json!({
            "msg_type": 7,
            "media": media,
            "msg_seq": self.seq.fetch_add(1, Ordering::Relaxed),
        });
        if let Some(id) = reply_to {
            body["msg_id"] = json!(id);
        }
        self.post_message(target, body).await
    }
}

/// 认证、限流、服务端错误不是 Markdown 问题，降级无意义。
fn markdown_degradable(status: u16) -> bool {
    !matches!(status, 401 | 403 | 429) && status < 500
}

fn message_payload(
    target: &Target,
    text: &str,
    markdown: bool,
    reply_to: Option<&str>,
    seq: u32,
) -> Value {
    let mut p = if markdown {
        json!({ "markdown": { "content": text } })
    } else {
        json!({ "content": text })
    };
    if target.msg_type.is_v2() {
        p["msg_type"] = json!(if markdown { 2 } else { 0 });
        p["msg_seq"] = json!(seq);
    }
    if let Some(id) = reply_to {
        p["msg_id"] = json!(id);
    }
    p
}

fn api_error(status: u16, path: &str, body: &str) -> ChannelError {
    #[derive(Deserialize)]
    struct Body {
        message: String,
    }
    let message = serde_json::from_str::<Body>(body)
        .map(|b| b.message)
        .unwrap_or_else(|_| body.chars().take(300).collect());
    ChannelError::Api {
        status,
        path: path.to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::Router;
    use axum::extract::State;
    use axum::http::{StatusCode, Uri};

    use super::*;

    #[test]
    fn binding_roundtrip() {
        let t = Target {
            msg_type: MsgType::Group,
            chat_id: "G123".into(),
        };
        assert_eq!(t.binding(), "qq:group:G123");
        assert_eq!(Target::from_binding("qq:group:G123").unwrap(), t);
        assert_eq!(
            Target::from_binding("qq:dm:99").unwrap().msg_type,
            MsgType::Dm
        );
    }

    #[test]
    fn binding_rejects_garbage() {
        for b in ["tg:group:1", "qq:group", "qq:foo:1", "qq:c2c:"] {
            assert!(Target::from_binding(b).is_err(), "{b}");
        }
    }

    #[test]
    fn upload_only_for_group_and_c2c() {
        let t = |msg_type| Target {
            msg_type,
            chat_id: "1".into(),
        };
        assert_eq!(t(MsgType::C2c).upload_path().unwrap(), "/v2/users/1/files");
        assert_eq!(
            t(MsgType::Group).upload_path().unwrap(),
            "/v2/groups/1/files"
        );
        for ty in [MsgType::Guild, MsgType::Dm] {
            assert!(matches!(
                t(ty).upload_path(),
                Err(ChannelError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn payload_shapes() {
        let group = Target {
            msg_type: MsgType::Group,
            chat_id: "g".into(),
        };
        let md = message_payload(&group, "hi", true, Some("m1"), 3);
        assert_eq!(
            md,
            json!({"markdown":{"content":"hi"},"msg_type":2,"msg_seq":3,"msg_id":"m1"})
        );
        let plain = message_payload(&group, "hi", false, None, 4);
        assert_eq!(plain, json!({"content":"hi","msg_type":0,"msg_seq":4}));
        let guild = Target {
            msg_type: MsgType::Guild,
            chat_id: "c".into(),
        };
        assert_eq!(
            message_payload(&guild, "hi", false, Some("m"), 1),
            json!({"content":"hi","msg_id":"m"})
        );
    }

    #[test]
    fn degrade_policy() {
        assert!(markdown_degradable(400));
        assert!(markdown_degradable(404));
        for s in [401, 403, 429, 500, 503] {
            assert!(!markdown_degradable(s));
        }
    }

    #[test]
    fn quote_id_prefers_ref_idx() {
        let s: SentMessage = serde_json::from_str(
            r#"{"id":"ROBOT1.0_x","timestamp":1,"ext_info":{"ref_idx":"REFIDX_1"}}"#,
        )
        .unwrap();
        assert_eq!(s.quote_id(), "REFIDX_1");
        let s: SentMessage = serde_json::from_str(r#"{"id":"abc"}"#).unwrap();
        assert_eq!(s.quote_id(), "abc");
    }

    type Log = Arc<StdMutex<Vec<(String, Value)>>>;

    /// 本地 OpenAPI 桩：记录请求；`markdown_status` 非 200 时拒绝 Markdown 报文。
    async fn stub(markdown_status: u16) -> (QqApi, Log) {
        #[derive(Clone)]
        struct St(Log, u16);
        async fn handle(
            State(St(log, md)): State<St>,
            uri: Uri,
            body: String,
        ) -> (StatusCode, String) {
            let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            log.lock()
                .unwrap()
                .push((uri.path().to_string(), body.clone()));
            if uri.path() == "/token" {
                return (
                    StatusCode::OK,
                    r#"{"access_token":"tok","expires_in":"7200"}"#.into(),
                );
            }
            if body.get("markdown").is_some() && md != 200 {
                return (
                    StatusCode::from_u16(md).unwrap(),
                    r#"{"message":"rejected","code":1}"#.into(),
                );
            }
            (StatusCode::OK, r#"{"id":"mid","timestamp":1}"#.into())
        }
        let log: Log = Default::default();
        let app = Router::new()
            .fallback(handle)
            .with_state(St(log.clone(), markdown_status));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut api = QqApi::new("app".into(), "secret".into());
        api.base_url = format!("http://{addr}");
        api.token_url = format!("http://{addr}/token");
        (api, log)
    }

    fn c2c() -> Target {
        Target {
            msg_type: MsgType::C2c,
            chat_id: "u1".into(),
        }
    }

    #[tokio::test]
    async fn markdown_rejection_falls_back_to_plain() {
        let (api, log) = stub(400).await;
        let ids = api.send_text(&c2c(), "**hi**", None).await.unwrap();
        assert_eq!(ids, ["mid"]);
        let log = log.lock().unwrap();
        let sent: Vec<_> = log.iter().filter(|(p, _)| p.starts_with("/v2")).collect();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].1["content"], "hi");
        assert_eq!(sent[1].1["msg_type"], 0);
        assert_eq!(sent[0].1["msg_seq"], sent[1].1["msg_seq"]);
    }

    #[tokio::test]
    async fn server_error_is_not_degraded() {
        let (api, log) = stub(500).await;
        let err = api.send_text(&c2c(), "hi", None).await.unwrap_err();
        assert!(matches!(err, ChannelError::Api { status: 500, .. }));
        assert_eq!(
            log.lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.starts_with("/v2"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn passive_reply_is_capped_and_announced() {
        let (api, log) = stub(200).await;
        let text = "x".repeat(MAX_MESSAGE_CHARS * 8);
        let ids = api.send_text(&c2c(), &text, Some("in1")).await.unwrap();
        assert_eq!(ids.len(), PASSIVE_REPLY_LIMIT);
        let log = log.lock().unwrap();
        let sent: Vec<_> = log.iter().filter(|(p, _)| p.starts_with("/v2")).collect();
        assert_eq!(sent.len(), PASSIVE_REPLY_LIMIT);
        assert!(sent.iter().all(|(_, b)| b["msg_id"] == "in1"));
        let last = sent.last().unwrap().1["markdown"]["content"]
            .as_str()
            .unwrap();
        assert!(last.ends_with("[回复过长，已截断。]"));
    }

    #[tokio::test]
    async fn media_upload_rejected_for_guild_before_network() {
        let api = QqApi::new("a".into(), "s".into());
        let t = Target {
            msg_type: MsgType::Guild,
            chat_id: "c".into(),
        };
        let err = api
            .send_media(&t, 1, "a.png", String::new(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::Unsupported(_)));
    }
}
