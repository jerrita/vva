//! QQ 私聊进度流：首次汇报创建消息，后续汇报保留正文前缀并追加。

use std::sync::Arc;

use async_trait::async_trait;

use super::api::{ProgressFrame, QqApi};
use crate::space::channel::{ProgressStream, Result};

pub(super) struct QqProgress {
    api: Arc<QqApi>,
    openid: String,
    reply_to: String,
    seq: u32,
    index: u32,
    stream_id: Option<String>,
    quote_id: Option<String>,
    text: String,
    failed: bool,
    finished: bool,
}

impl QqProgress {
    pub fn new(api: Arc<QqApi>, openid: String, reply_to: String) -> Self {
        let seq = api.next_seq();
        Self {
            api,
            openid,
            reply_to,
            seq,
            index: 0,
            stream_id: None,
            quote_id: None,
            text: String::new(),
            failed: false,
            finished: false,
        }
    }

    fn appended(&self, text: &str) -> String {
        if self.text.is_empty() {
            text.to_string()
        } else {
            format!("{}\n\n{text}", self.text)
        }
    }

    async fn send(&mut self, text: String, finished: bool) -> Result<Option<String>> {
        let index = self.index;
        self.index += 1;
        let sent = self
            .api
            .send_progress(
                &self.openid,
                ProgressFrame {
                    text: &text,
                    reply_to: &self.reply_to,
                    stream_id: self.stream_id.as_deref(),
                    seq: self.seq,
                    index,
                    finished,
                },
            )
            .await?;
        self.stream_id = Some(sent.id.clone());
        self.text = text;
        let quote_id = sent.quote_id();
        if self.quote_id.as_ref() == Some(&quote_id) {
            return Ok(None);
        }
        self.quote_id = Some(quote_id.clone());
        Ok(Some(quote_id))
    }
}

#[async_trait]
impl ProgressStream for QqProgress {
    async fn append(&mut self, text: &str) -> Result<Option<String>> {
        if self.failed || self.finished || text.is_empty() {
            return Ok(None);
        }
        let result = self.send(self.appended(text), false).await;
        self.failed = result.is_err();
        result
    }

    async fn finish(&mut self, status: &str) -> Result<Option<String>> {
        if self.finished {
            return Ok(None);
        }
        self.finished = true;
        if self.stream_id.is_none() {
            return Ok(None);
        }
        self.send(self.appended(status), true).await
    }
}
