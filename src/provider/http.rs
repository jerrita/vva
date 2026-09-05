//! 供应商 HTTP 传输与 SSE 流式事件循环的共享封装。
//!
//! 各 Provider 仅声明端点 URL、鉴权 Header 与事件语义，
//! 请求发送、非 2xx 错误转换与 SSE 解码循环在此统一实现。

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::HeaderMap;

use super::{ProviderError, sse::SseDecoder};

/// SSE 事件处理器：每个 Provider 用自身流式状态实现该接口。
/// 异步接口保留 `send().await` 的背压语义，避免缓冲溢出丢事件。
#[async_trait]
pub trait SseEventHandler: Send {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError>;
}

/// 发送 JSON POST 并解析 JSON 响应（非 2xx 返回 ApiError）
pub async fn post_json(
    url: &str,
    headers: HeaderMap,
    body: &serde_json::Value,
) -> Result<serde_json::Value, ProviderError> {
    ensure_success(
        super::shared_client()
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await?,
    )
    .await?
    .json()
    .await
    .map_err(Into::into)
}

/// 发送 JSON POST 并将 SSE 事件逐个交给处理器（自动补 Accept 头）
pub async fn post_sse<H: SseEventHandler>(
    url: &str,
    mut headers: HeaderMap,
    body: &serde_json::Value,
    handler: &mut H,
) -> Result<(), ProviderError> {
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    let res = ensure_success(
        super::shared_client()
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await?,
    )
    .await?;
    consume_sse(res, handler).await
}

async fn ensure_success(res: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(ProviderError::ApiError { status, body });
    }
    Ok(res)
}

/// 消费 SSE 流并将每个事件 JSON 交给处理器。
/// "[DONE]" 为 OpenAI 风格流终止哨兵（Anthropic / Gemini 不发送），收到即终止；
/// 无法解析的事件数据按协议错误上报（仅跳过空数据行），流自然结束后 flush 解码器残留事件。
async fn consume_sse<H: SseEventHandler>(
    res: reqwest::Response,
    handler: &mut H,
) -> Result<(), ProviderError> {
    let mut stream = res.bytes_stream();
    let mut decoder = SseDecoder::new();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        for ev in decoder.decode(&chunk) {
            if ev.data == "[DONE]" {
                return Ok(());
            }
            dispatch_event(&ev.data, handler).await?;
        }
    }

    if let Some(ev) = decoder.flush()
        && ev.data != "[DONE]"
    {
        dispatch_event(&ev.data, handler).await?;
    }

    Ok(())
}

async fn dispatch_event<H: SseEventHandler>(
    data: &str,
    handler: &mut H,
) -> Result<(), ProviderError> {
    if data.trim().is_empty() {
        return Ok(());
    }
    let json = serde_json::from_str::<serde_json::Value>(data)
        .map_err(|e| ProviderError::Protocol(format!("Invalid JSON in SSE event data: {e}")))?;
    handler.on_event(json).await
}
