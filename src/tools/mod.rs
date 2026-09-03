pub mod bash;
pub mod file;

use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use crate::model::{ToolCall, ToolDefinition, ToolResult};

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
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
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

