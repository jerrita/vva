//! 内置 web fetch provider：HTTP 抓取 + 正文提取（实现参考 defuddle：
//! 剥离页面噪声、取主内容），Rust 侧用 dom_smoothie（Readability 算法）提取、htmd 转 Markdown。

use std::time::Duration;

use async_trait::async_trait;

use super::WebFetchContent;
use crate::provider::shared_client;

/// 单页抓取超时（shared client 的读空闲超时为 SSE 设计，此处收紧为总量超时）
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// 响应体字节上限：超限截断（正文提取对截断后的 HTML 仍可工作）
const MAX_BODY_BYTES: usize = 3 * 1024 * 1024;
/// 以浏览器 UA 抓取，避免被反爬策略拦截
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

pub struct LocalFetchProvider;

#[async_trait]
impl super::WebFetchProvider for LocalFetchProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn fetch(&self, url: &str) -> Result<WebFetchContent, String> {
        let response = shared_client()
            .get(url)
            .timeout(FETCH_TIMEOUT)
            .header(reqwest::header::USER_AGENT, BROWSER_USER_AGENT)
            .send()
            .await
            .map_err(|error| format!("Failed to fetch {url}: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("Failed to fetch {url} (status {status})"));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let body = read_body_limited(response).await?;
        if content_type.contains("text/html") || content_type.contains("application/xhtml") {
            extract_html(&body, url)
        } else if content_type.starts_with("text/") || content_type.is_empty() {
            Ok(WebFetchContent {
                title: String::new(),
                content: body,
            })
        } else {
            Err(format!(
                "web_fetch does not support content type '{content_type}': {url}"
            ))
        }
    }
}

/// 分块读取响应体，超过字节上限即截断
async fn read_body_limited(mut response: reqwest::Response) -> Result<String, String> {
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed to read response body: {error}"))?
    {
        bytes.extend_from_slice(&chunk);
        if bytes.len() >= MAX_BODY_BYTES {
            bytes.truncate(MAX_BODY_BYTES);
            break;
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// HTML → 主内容 Markdown：dom_smoothie 提取正文，htmd 转换
fn extract_html(html: &str, url: &str) -> Result<WebFetchContent, String> {
    let mut readability = dom_smoothie::Readability::new(html, Some(url), None)
        .map_err(|error| format!("Failed to parse HTML from {url}: {error}"))?;
    let article = readability
        .parse()
        .map_err(|error| format!("Failed to extract main content from {url}: {error}"))?;
    let markdown = htmd::convert(&article.content)
        .map_err(|error| format!("Failed to convert HTML to Markdown from {url}: {error}"))?;
    Ok(WebFetchContent {
        title: article.title,
        content: markdown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_main_content_dropping_page_chrome() {
        const SAMPLE: &str = r#"<html>
            <head><title>Sample Post</title></head>
            <body>
                <nav>nav-noise-text</nav>
                <script>var tracker = 1;</script>
                <article>
                    <h1>Main Heading</h1>
                    <p>unique-main-paragraph-content for the article body.</p>
                </article>
                <footer>footer-noise-text</footer>
            </body>
        </html>"#;
        let content = extract_html(SAMPLE, "https://example.com/post").unwrap();

        assert!(content.content.contains("Main Heading"));
        assert!(content.content.contains("unique-main-paragraph-content"));
        assert!(!content.content.contains("nav-noise-text"));
        assert!(!content.content.contains("tracker"));
    }
}
