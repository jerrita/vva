use async_trait::async_trait;
use serde::Deserialize;

use crate::model::ToolInput;
use crate::provider::CompletionOptions;
use crate::provider::anthropic::AnthropicProvider;
use crate::tools::{Tool, parse_args};

pub(crate) struct StandaloneSearchTool {
    provider: AnthropicProvider,
    options: CompletionOptions,
}

impl StandaloneSearchTool {
    pub(crate) fn new(provider: AnthropicProvider, options: CompletionOptions) -> Self {
        Self { provider, options }
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
