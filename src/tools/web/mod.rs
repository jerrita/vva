//! 外置 web search / fetch provider：为原生 server tool 能力缺位的模型提供客户端工具补全。
//! 能力矩阵：`brave` / `zhipu` 仅 search；`google` 二者皆支持（单模型原生 google_search / url_context）；`local` 仅 fetch（内置，无需声明）；`tavily` / `deepseek` 二者皆支持。

pub mod brave;
pub mod deepseek;
pub mod google;
pub mod local;
pub(crate) mod standalone;
pub mod tavily;
pub mod tool;
pub mod zhipu;

use std::sync::Arc;

use async_trait::async_trait;

/// 一条 web search 结果
#[derive(Debug)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 一次 web fetch 的页面正文
#[derive(Debug)]
pub struct WebFetchContent {
    pub title: String,
    pub content: String,
}

#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    /// provider 名（Notice 与激活提示用）
    fn name(&self) -> &'static str;

    async fn search(&self, query: &str, max_results: usize)
    -> Result<Vec<WebSearchResult>, String>;
}

#[async_trait]
pub trait WebFetchProvider: Send + Sync {
    fn name(&self) -> &'static str;

    async fn fetch(&self, url: &str) -> Result<WebFetchContent, String>;
}

/// 按 `settings.web` 解析出的外置 provider 集合；未选择的能力位为 `None`
pub struct ExternalWeb {
    pub search: Option<Arc<dyn WebSearchProvider>>,
    pub fetch: Option<Arc<dyn WebFetchProvider>>,
}
