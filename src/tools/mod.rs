pub mod bash;
pub mod file;
pub mod powershell;

use crate::model::{ToolCall, ToolDefinition, ToolResult};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;
    async fn execute(&self, arguments: &str) -> Result<String, String>;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }
}

pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub async fn execute_call(&self, call: &ToolCall) -> ToolResult {
        match self.get(&call.name) {
            Some(tool) => match tool.execute(&call.arguments).await {
                Ok(content) => ToolResult::success(&call.id, content),
                Err(err) => ToolResult::error(&call.id, err),
            },
            None => ToolResult::error(&call.id, format!("Tool not found: {}", call.name)),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NamedTool(&'static str);

    #[async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn execute(&self, _arguments: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    #[test]
    fn definitions_have_stable_name_order() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(NamedTool("powershell")));
        registry.register(Arc::new(NamedTool("file_view")));
        registry.register(Arc::new(NamedTool("file_write")));

        let names: Vec<_> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();

        assert_eq!(names, ["file_view", "file_write", "powershell"]);
    }
}
