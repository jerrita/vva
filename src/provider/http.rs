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
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    body: &serde_json::Value,
) -> Result<serde_json::Value, ProviderError> {
    ensure_success(client.post(url).headers(headers).json(body).send().await?)
        .await?
        .json()
        .await
        .map_err(Into::into)
}

/// 发送 JSON POST 并返回 SSE 流式响应（非 2xx 返回 ApiError）
pub async fn post_sse(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    body: &serde_json::Value,
) -> Result<reqwest::Response, ProviderError> {
    ensure_success(client.post(url).headers(headers).json(body).send().await?).await
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
/// 跳过 "[DONE]" 哨兵与无法解析的事件行，流结束后 flush 解码器残留事件。
pub async fn consume_sse<H: SseEventHandler>(
    res: reqwest::Response,
    handler: &mut H,
) -> Result<(), ProviderError> {
    let mut stream = res.bytes_stream();
    let mut decoder = SseDecoder::new();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        for ev in decoder.decode(&chunk) {
            if ev.data == "[DONE]" {
                break;
            }
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                handler.on_event(json).await?;
            }
        }
    }

    if let Some(ev) = decoder.flush()
        && ev.data != "[DONE]"
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&ev.data)
    {
        handler.on_event(json).await?;
    }

    Ok(())
}
