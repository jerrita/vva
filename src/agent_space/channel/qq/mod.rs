//! QQ 机器人渠道（官方 OpenAPI + 网关 WebSocket）。

mod api;
mod event;
mod gateway;
mod text;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tokio::sync::mpsc;

use self::api::{QqApi, Target};
use self::event::{Attachment, MessageEvent};
use self::gateway::Gateway;
use super::files::{UrlSource, normalize_url};
use super::{
    Channel, ChannelError, Inbound, InboundFile, Outbound, OutboundFile, OutboundText, Result,
};

pub use self::api::MsgType;

/// 事件去重窗口：QQ 偶发重复推送同一 msg_id。
const SEEN_WINDOW: usize = 256;
const MEDIA_IMAGE: u8 = 1;
const MEDIA_VIDEO: u8 = 2;
const MEDIA_AUDIO: u8 = 3;
const MEDIA_FILE: u8 = 4;

/// QQ 渠道凭据与准入。
#[derive(Debug, Clone)]
pub struct QqOptions {
    pub app_id: String,
    pub secret: String,
    /// 允许触达 agent 的用户 id（群为 member_openid，单聊为 user_openid，频道为 author.id）。
    /// 为空表示拒绝所有人；含 `"*"` 表示允许任何用户（该机器人可执行 agent 工具，慎用）。
    pub allowed_users: Vec<String>,
}

pub struct QqChannel {
    api: Arc<QqApi>,
    allowed_users: Vec<String>,
}

impl QqChannel {
    pub fn new(options: QqOptions) -> Self {
        Self {
            api: Arc::new(QqApi::new(options.app_id, options.secret)),
            allowed_users: options.allowed_users,
        }
    }

    /// 白名单含 `"*"` 表示允许任何用户；空白名单拒绝所有人
    fn allows(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// 白名单外丢弃（warn）；无文字无附件丢弃；其余转为入站消息。
    fn admit(&self, ev: MessageEvent) -> Option<Inbound> {
        let binding = ev.target.binding();
        if !self.allows(&ev.user_id) {
            tracing::warn!(
                "QQ 白名单外用户消息已丢弃: user={} binding={binding}",
                ev.user_id
            );
            return None;
        }
        if ev.text.is_empty() && ev.attachments.is_empty() {
            tracing::debug!("QQ 空消息已忽略: binding={binding}");
            return None;
        }
        tracing::info!(
            "QQ 入站事件: binding={binding} user={} 文字 {} 字符, 附件 {}",
            ev.user_id,
            ev.text.chars().count(),
            ev.attachments.len()
        );
        let files = ev
            .attachments
            .into_iter()
            .map(|a| self.inbound_file(a))
            .collect();
        Some(Inbound {
            binding,
            user_id: ev.user_id,
            message_id: ev.message_id,
            text: ev.text,
            files,
            reply_to: ev.reply_to,
        })
    }

    fn inbound_file(&self, a: Attachment) -> InboundFile {
        let url = normalize_url(&a.url);
        let filename = a
            .filename
            .filter(|n| !n.is_empty())
            .or_else(|| {
                let path = url.split(['?', '#']).next()?;
                path.rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            })
            .unwrap_or_else(|| "file".into());
        let source = UrlSource {
            client: self.api.http().clone(),
            url,
        };
        InboundFile::new(filename, a.content_type, a.size, source)
    }

    async fn send_text(&self, m: OutboundText) -> Result<Vec<String>> {
        let target = Target::from_binding(&m.binding)?;
        let ids = self
            .api
            .send_text(&target, &m.text, m.reply_to.as_deref())
            .await?;
        tracing::debug!(
            "QQ 出站文本: binding={} {} 字符, 返回 id {} 个",
            m.binding,
            m.text.chars().count(),
            ids.len()
        );
        Ok(ids)
    }

    async fn send_file(&self, m: OutboundFile) -> Result<Vec<String>> {
        let target = Target::from_binding(&m.binding)?;
        // 先于 caption 与读文件校验，不支持时不产生半截发送。
        target.upload_path()?;
        let file_type = media_type(&m.path);
        let name = m
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        if file_type == MEDIA_FILE {
            tracing::warn!("QQ 富媒体官方仅承诺图片/视频/语音，尝试通用文件上传: {name}");
        }
        let mut ids = Vec::new();
        if !m.caption.trim().is_empty() {
            ids.extend(
                self.api
                    .send_text(&target, &m.caption, m.reply_to.as_deref())
                    .await?,
            );
        }
        let data = STANDARD.encode(tokio::fs::read(&m.path).await?);
        let file_name = super::files::sanitize_filename(name);
        ids.push(
            self.api
                .send_media(&target, file_type, &file_name, data, m.reply_to.as_deref())
                .await?,
        );
        tracing::debug!(
            "QQ 出站文件: binding={} file_type={file_type} 返回 id {} 个",
            m.binding,
            ids.len()
        );
        Ok(ids)
    }
}

/// 按扩展名选 file_type：1 图片 / 2 视频 / 3 语音 / 4 文件。
fn media_type(path: &Path) -> u8 {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp") => MEDIA_IMAGE,
        Some("mp4") => MEDIA_VIDEO,
        Some("silk") => MEDIA_AUDIO,
        _ => MEDIA_FILE,
    }
}

#[async_trait]
impl Channel for QqChannel {
    fn name(&self) -> &'static str {
        "qq"
    }

    async fn send(&self, msg: Outbound) -> Result<Vec<String>> {
        match msg {
            Outbound::Text(m) => self.send_text(m).await,
            Outbound::File(m) => self.send_file(m).await,
        }
    }

    async fn run(self: Arc<Self>, inbound: mpsc::Sender<Inbound>) -> Result<()> {
        let (tx, mut rx) = mpsc::channel(64);
        let gateway = tokio::spawn(Gateway::new(self.api.clone(), tx).run());
        let mut seen: VecDeque<String> = VecDeque::with_capacity(SEEN_WINDOW);
        while let Some(ev) = rx.recv().await {
            if seen.contains(&ev.message_id) {
                tracing::debug!("QQ 重复事件已忽略: {}", ev.message_id);
                continue;
            }
            if seen.len() == SEEN_WINDOW {
                seen.pop_front();
            }
            seen.push_back(ev.message_id.clone());
            if let Some(msg) = self.admit(ev)
                && inbound.send(msg).await.is_err()
            {
                break;
            }
        }
        gateway.abort();
        match gateway.await {
            Err(e) if e.is_panic() => Err(ChannelError::Protocol(format!("QQ 网关任务崩溃: {e}"))),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::event::parse_message_event;
    use super::*;

    fn channel(allowed: &[&str]) -> QqChannel {
        QqChannel::new(QqOptions {
            app_id: "app".into(),
            secret: "secret".into(),
            allowed_users: allowed.iter().map(|s| s.to_string()).collect(),
        })
    }

    fn group_event(member: &str, content: &str, attachments: serde_json::Value) -> MessageEvent {
        parse_message_event(
            "GROUP_AT_MESSAGE_CREATE",
            serde_json::json!({
                "author": {"id": "x", "member_openid": member},
                "content": content, "group_openid": "GRP", "id": "m1",
                "attachments": attachments,
            }),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn allowlisted_user_is_admitted() {
        let ch = channel(&["MEM"]);
        let msg = ch
            .admit(group_event("MEM", " 你好", serde_json::json!([])))
            .unwrap();
        assert_eq!(msg.binding, "qq:group:GRP");
        assert_eq!(msg.user_id, "MEM");
        assert_eq!(msg.message_id, "m1");
        assert_eq!(msg.text, "你好");
    }

    #[test]
    fn stranger_and_empty_allowlist_are_dropped() {
        assert!(
            channel(&["MEM"])
                .admit(group_event("EVIL", "hi", serde_json::json!([])))
                .is_none()
        );
        assert!(
            channel(&[])
                .admit(group_event("MEM", "hi", serde_json::json!([])))
                .is_none()
        );
    }

    #[test]
    fn wildcard_allows_any_user() {
        let ch = channel(&["*"]);
        assert!(
            ch.admit(group_event("ANYONE", "hi", serde_json::json!([])))
                .is_some()
        );
    }

    #[test]
    fn empty_message_is_dropped() {
        assert!(
            channel(&["MEM"])
                .admit(group_event("MEM", "  ", serde_json::json!([])))
                .is_none()
        );
    }

    #[test]
    fn attachments_get_scheme_and_filename() {
        let att = serde_json::json!([
            {"content_type": "image/png", "filename": "a.png", "size": 3, "url": "cdn.qq.com/a.png"},
            {"content_type": "application/pdf", "size": 0, "url": "//cdn.qq.com/dl/b.pdf?x=1"},
        ]);
        let msg = channel(&["MEM"])
            .admit(group_event("MEM", "", att))
            .unwrap();
        assert_eq!(msg.files.len(), 2);
        assert_eq!(msg.files[0].filename, "a.png");
        assert_eq!(msg.files[0].mime, "image/png");
        assert_eq!(msg.files[0].size, 3);
        assert_eq!(msg.files[1].filename, "b.pdf");
        assert!(format!("{:?}", msg.files[1]).contains("https://cdn.qq.com/dl/b.pdf?x=1"));
    }

    #[test]
    fn media_types() {
        let t = |p: &str| media_type(Path::new(p));
        assert_eq!(t("a.PNG"), 1);
        assert_eq!(t("a.mp4"), 2);
        assert_eq!(t("a.silk"), 3);
        assert_eq!(t("a.pdf"), 4);
        assert_eq!(t("noext"), 4);
    }

    #[tokio::test]
    async fn file_to_guild_is_rejected_up_front() {
        let ch = channel(&[]);
        let err = ch
            .send(Outbound::File(OutboundFile {
                binding: "qq:guild:C1".into(),
                path: "/nonexistent/a.png".into(),
                caption: "cap".into(),
                reply_to: None,
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::Unsupported(_)));
    }
}
