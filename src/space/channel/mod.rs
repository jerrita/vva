//! Channel 抽象：IM 渠道与 agent space 之间的入站 / 出站消息边界。
//!
//! 入站由渠道推送 [`Inbound`]；出站由调用方（出站渲染器）经 [`Channel::send`] 投递。
//! 渠道只负责协议适配、鉴权白名单与分段发送，不感知 session 路由。

pub mod files;
pub mod qq;

use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

/// Channel 层统一错误。
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    /// 平台 API 返回非 2xx。
    #[error("{path} -> {status}: {message}")]
    Api {
        status: u16,
        path: String,
        message: String,
    },
    /// 平台报文与官方协议不符。
    #[error("协议错误: {0}")]
    Protocol(String),
    /// 该渠道 / 聊天类型不支持的操作。
    #[error("不支持: {0}")]
    Unsupported(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("websocket: {0}")]
    Ws(String),
}

pub type Result<T> = std::result::Result<T, ChannelError>;

/// 用户引用（回复）的消息。`message_id` 与 [`Channel::send`] 返回的 id 同一命名空间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyRef {
    pub message_id: String,
    pub text: String,
}

/// 入站消息。`binding` 形如 `qq:<msg_type>:<chat_id>`。
#[derive(Debug, Clone)]
pub struct Inbound {
    pub binding: String,
    pub user_id: String,
    pub message_id: String,
    pub text: String,
    pub files: Vec<InboundFile>,
    pub reply_to: Option<ReplyRef>,
}

/// 入站附件的下载来源（渠道实现）。
#[async_trait]
pub trait FileSource: Send + Sync + Debug {
    /// 把附件完整写入 `dest`（不设体积上限）。失败时不留下半截文件。
    async fn download(&self, dest: &Path) -> Result<()>;
}

/// 入站附件：元数据即时可用，内容按需下载。
#[derive(Debug, Clone)]
pub struct InboundFile {
    pub filename: String,
    pub mime: String,
    source: Arc<dyn FileSource>,
}

impl InboundFile {
    pub fn new(filename: String, mime: String, source: impl FileSource + 'static) -> Self {
        Self {
            filename,
            mime,
            source: Arc::new(source),
        }
    }

    pub async fn download(&self, dest: &Path) -> Result<()> {
        self.source.download(dest).await
    }
}

/// 出站文本（Markdown）。`reply_to` 为被动回复的入站 `message_id`。
#[derive(Debug, Clone)]
pub struct OutboundText {
    pub binding: String,
    pub text: String,
    pub reply_to: Option<String>,
}

/// 出站文件；`caption` 非空时先作为独立文本消息发送。
#[derive(Debug, Clone)]
pub struct OutboundFile {
    pub binding: String,
    pub path: PathBuf,
    pub caption: String,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Outbound {
    Text(OutboundText),
    File(OutboundFile),
}

/// 一轮工作的进度消息；每次追加均更新同一条渠道消息。
/// 返回新出现的引用 id，供调用方关联会话；未创建消息时结束不发送。
#[async_trait]
pub trait ProgressStream: Send {
    async fn append(&mut self, text: &str) -> Result<Option<String>>;
    async fn finish(&mut self, status: &str) -> Result<Option<String>>;
}

#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// 渠道名，同时是 binding 前缀。
    fn name(&self) -> &'static str;

    /// 发送出站消息，返回已发出消息的 id（后续入站 `reply_to.message_id` 引用的即此 id）。
    async fn send(&self, msg: Outbound) -> Result<Vec<String>>;

    /// 为被动回复创建进度流；不支持的聊天类型返回 `None`。
    /// 实际消息在首次追加时创建。
    fn progress_stream(
        &self,
        _binding: &str,
        _reply_to: &str,
    ) -> Result<Option<Box<dyn ProgressStream>>> {
        Ok(None)
    }

    /// 收消息主循环（含断线重连）；入站接收端关闭时正常返回。
    async fn run(self: Arc<Self>, inbound: mpsc::Sender<Inbound>) -> Result<()>;
}
