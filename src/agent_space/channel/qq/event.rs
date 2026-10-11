//! QQ 网关消息事件解析（官方 v2 事件载荷 → 领域事件）。

use serde::Deserialize;
use serde_json::Value;

use super::api::{MsgType, Target};
use crate::agent_space::channel::{ChannelError, ReplyRef, Result};

/// 引用消息的 `message_type`（官方消息类型表：103 = 引用）。
const MESSAGE_TYPE_QUOTE: u32 = 103;

/// 附件（官方 attachments 条目）。
#[derive(Debug, Deserialize)]
pub struct Attachment {
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub size: u64,
    /// 原始 URL，可能缺协议头。
    pub url: String,
}

#[derive(Debug)]
pub struct MessageEvent {
    pub target: Target,
    pub user_id: String,
    pub message_id: String,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<ReplyRef>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: String,
    #[serde(default)]
    content: String,
    author: RawAuthor,
    group_openid: Option<String>,
    channel_id: Option<String>,
    guild_id: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
    message_type: Option<u32>,
    #[serde(default)]
    msg_elements: Vec<MsgElement>,
    message_scene: Option<MessageScene>,
    message_reference: Option<MessageReference>,
}

#[derive(Deserialize)]
struct RawAuthor {
    id: Option<String>,
    member_openid: Option<String>,
    user_openid: Option<String>,
}

#[derive(Deserialize)]
struct MsgElement {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct MessageScene {
    /// `["msg_idx=REFIDX_x", "ref_msg_idx=REFIDX_y"]`。
    #[serde(default)]
    ext: Vec<String>,
}

#[derive(Deserialize)]
struct MessageReference {
    message_id: String,
}

/// 消息类事件名 → 聊天类型。
fn message_kind(event: &str) -> Option<MsgType> {
    match event {
        "GROUP_AT_MESSAGE_CREATE" => Some(MsgType::Group),
        "C2C_MESSAGE_CREATE" => Some(MsgType::C2c),
        "AT_MESSAGE_CREATE" => Some(MsgType::Guild),
        "DIRECT_MESSAGE_CREATE" => Some(MsgType::Dm),
        _ => None,
    }
}

/// 解析消息类事件；非消息事件返回 `Ok(None)`。
pub fn parse_message_event(event: &str, data: Value) -> Result<Option<MessageEvent>> {
    let Some(msg_type) = message_kind(event) else {
        return Ok(None);
    };
    let raw: RawMessage = serde_json::from_value(data)
        .map_err(|e| ChannelError::Protocol(format!("{event} 载荷非法: {e}")))?;
    let reply_to = reply_ref(&raw);
    let require = |field: Option<String>, name: &str| {
        field.ok_or_else(|| ChannelError::Protocol(format!("{event} 缺少 {name}")))
    };
    let author = raw.author;
    // chat id 与用户 id 的来源字段随事件类型而定；单聊的 chat id 即用户 openid。
    let (chat_id, user_id) = match msg_type {
        MsgType::Group => (
            require(raw.group_openid, "group_openid")?,
            require(author.member_openid, "author.member_openid")?,
        ),
        MsgType::C2c => {
            let user = require(author.user_openid, "author.user_openid")?;
            (user.clone(), user)
        }
        MsgType::Guild => (
            require(raw.channel_id, "channel_id")?,
            require(author.id, "author.id")?,
        ),
        MsgType::Dm => (
            require(raw.guild_id, "guild_id")?,
            require(author.id, "author.id")?,
        ),
    };
    Ok(Some(MessageEvent {
        target: Target { msg_type, chat_id },
        user_id,
        message_id: raw.id,
        text: raw.content.trim().to_string(),
        attachments: raw.attachments,
        reply_to,
    }))
}

/// v2 引用：`message_type == 103`，被引正文在 `msg_elements[0].content`，
/// 被引消息索引在 `message_scene.ext` 的 `ref_msg_idx=`；频道消息为 `message_reference.message_id`（无正文）。
fn reply_ref(raw: &RawMessage) -> Option<ReplyRef> {
    if let Some(r) = &raw.message_reference {
        return Some(ReplyRef {
            message_id: r.message_id.clone(),
            text: String::new(),
        });
    }
    if raw.message_type != Some(MESSAGE_TYPE_QUOTE) {
        return None;
    }
    let idx = raw
        .message_scene
        .as_ref()?
        .ext
        .iter()
        .find_map(|e| e.strip_prefix("ref_msg_idx="))?;
    let text = raw
        .msg_elements
        .first()
        .map(|e| e.content.trim().to_string());
    Some(ReplyRef {
        message_id: idx.to_string(),
        text: text.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn group_at_message() {
        let data = json!({
            "author": {"id": "A1", "member_openid": "MEM1"},
            "content": " 看看这个",
            "group_id": "GID", "group_openid": "GRP1",
            "id": "ROBOT1.0_abc",
            "timestamp": "2025-01-01T00:00:00+08:00",
            "attachments": [{
                "content_type": "image/png", "filename": "a.png",
                "height": 1, "width": 1, "size": 12,
                "url": "multimedia.nt.qq.com.cn/download?x=1"
            }]
        });
        let ev = parse_message_event("GROUP_AT_MESSAGE_CREATE", data)
            .unwrap()
            .unwrap();
        assert_eq!(ev.target.binding(), "qq:group:GRP1");
        assert_eq!(ev.user_id, "MEM1");
        assert_eq!(ev.message_id, "ROBOT1.0_abc");
        assert_eq!(ev.text, "看看这个");
        assert_eq!(ev.attachments.len(), 1);
        assert_eq!(ev.attachments[0].filename.as_deref(), Some("a.png"));
        assert_eq!(ev.attachments[0].size, 12);
        assert!(ev.reply_to.is_none());
    }

    #[test]
    fn c2c_quote_message() {
        let data = json!({
            "author": {"id": "A", "union_openid": "U", "user_openid": "USR1"},
            "content": "这个是什么意思",
            "id": "ROBOT1.0_def",
            "timestamp": "2025-01-01T00:00:00+08:00",
            "message_type": 103,
            "msg_elements": [{"content": "被引用的话", "author": {"id": "B"}}],
            "message_scene": {"source": "default", "ext": ["msg_idx=REFIDX_new", "ref_msg_idx=REFIDX_old"]}
        });
        let ev = parse_message_event("C2C_MESSAGE_CREATE", data)
            .unwrap()
            .unwrap();
        assert_eq!(ev.target.binding(), "qq:c2c:USR1");
        assert_eq!(ev.user_id, "USR1");
        assert_eq!(
            ev.reply_to,
            Some(ReplyRef {
                message_id: "REFIDX_old".into(),
                text: "被引用的话".into()
            })
        );
    }

    #[test]
    fn plain_c2c_has_no_reply() {
        let data = json!({
            "author": {"user_openid": "USR1"}, "content": "hi", "id": "m",
            "message_scene": {"source": "default", "ext": ["msg_idx=REFIDX_x"]}
        });
        let ev = parse_message_event("C2C_MESSAGE_CREATE", data)
            .unwrap()
            .unwrap();
        assert!(ev.reply_to.is_none());
    }

    #[test]
    fn guild_and_dm_events() {
        let guild = json!({
            "author": {"id": "U9", "username": "n", "bot": false},
            "channel_id": "CH1", "guild_id": "G1", "content": "<@!1> hi", "id": "gm1",
            "message_reference": {"message_id": "prev"}
        });
        let ev = parse_message_event("AT_MESSAGE_CREATE", guild)
            .unwrap()
            .unwrap();
        assert_eq!(ev.target.binding(), "qq:guild:CH1");
        assert_eq!(ev.reply_to.unwrap().message_id, "prev");

        let dm = json!({"author": {"id": "U9"}, "channel_id": "CH2", "guild_id": "DMG", "content": "x", "id": "dm1"});
        let ev = parse_message_event("DIRECT_MESSAGE_CREATE", dm)
            .unwrap()
            .unwrap();
        assert_eq!(ev.target.binding(), "qq:dm:DMG");
    }

    #[test]
    fn malformed_and_foreign_events() {
        let no_group = json!({"author": {"member_openid": "M"}, "content": "x", "id": "1"});
        assert!(parse_message_event("GROUP_AT_MESSAGE_CREATE", no_group).is_err());
        assert!(
            parse_message_event("GROUP_ADD_ROBOT", json!({}))
                .unwrap()
                .is_none()
        );
    }
}
