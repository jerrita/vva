use async_trait::async_trait;
use serde::Deserialize;

use crate::model::ToolInput;
use crate::provider::CompletionOptions;
use crate::provider::anthropic::AnthropicProvider;
use crate::tools::{Tool, ToolRegistry, parse_args};

pub struct StandaloneSearchTool {
    provider: AnthropicProvider,
    options: CompletionOptions,
}

impl StandaloneSearchTool {
    pub fn new(provider: AnthropicProvider, options: CompletionOptions) -> Self {
        Self { provider, options }
    }

    /// 安装独立搜索：注册客户端工具、关闭主请求原生搜索并清除同名服务端预留。
    /// 搜索实现由宿主先行选定；本函数替换已有的同名客户端工具。
    pub fn install(
        provider: &AnthropicProvider,
        options: &mut CompletionOptions,
        tools: &mut ToolRegistry,
    ) {
        if !options.enable_web_search {
            return;
        }
        let search_options = options.clone();
        options.enable_web_search = false;
        tools.register(std::sync::Arc::new(Self::new(
            provider.clone(),
            search_options,
        )));
        tools.server_tools.remove("web_search");
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
}

#[async_trait]
impl Tool for StandaloneSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for current information. Returns search text and source links. Cite the relevant sources using Markdown links in your response."
    }

    fn input(&self) -> ToolInput {
        ToolInput::Json(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "The web search query."}
            },
            "required": ["query"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: SearchArgs = parse_args(arguments, self.name())?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err("query must not be empty".into());
        }
        self.provider
            .standalone_search(query, &self.options)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ExistingSearch;

    #[async_trait::async_trait]
    impl Tool for ExistingSearch {
        fn name(&self) -> &str {
            "web_search"
        }

        fn description(&self) -> &str {
            "existing search"
        }

        fn input(&self) -> ToolInput {
            ToolInput::Json(serde_json::json!({"type": "object"}))
        }

        async fn execute(&self, _: &str) -> Result<String, String> {
            unreachable!()
        }
    }

    #[test]
    fn install_switches_search_surface_without_rewriting_other_options() {
        let provider = AnthropicProvider::new("test");
        let mut options = CompletionOptions {
            enable_web_search: true,
            enable_web_fetch: false,
            web_tool_version: Some("20250305".into()),
            ..Default::default()
        };
        let mut tools = ToolRegistry::new();
        tools.register(std::sync::Arc::new(ExistingSearch));
        tools.reserve_server_tools(["web_search", "web_fetch"]);

        StandaloneSearchTool::install(&provider, &mut options, &mut tools);

        assert!(!options.enable_web_search);
        assert!(!options.enable_web_fetch);
        assert_eq!(options.web_tool_version.as_deref(), Some("20250305"));
        assert!(!tools.is_server_tool("web_search"));
        assert!(tools.is_server_tool("web_fetch"));
        assert_eq!(tools.definitions().len(), 1);
        assert_ne!(
            tools.get("web_search").unwrap().description(),
            "existing search"
        );
    }

    #[test]
    fn install_is_noop_when_search_is_disabled() {
        let provider = AnthropicProvider::new("test");
        let mut options = CompletionOptions {
            enable_web_search: false,
            web_tool_version: Some("20250305".into()),
            ..Default::default()
        };
        let mut tools = ToolRegistry::new();

        StandaloneSearchTool::install(&provider, &mut options, &mut tools);

        assert!(!options.enable_web_search);
        assert!(options.enable_web_fetch);
        assert_eq!(options.web_tool_version.as_deref(), Some("20250305"));
        assert!(tools.get("web_search").is_none());
    }
}
