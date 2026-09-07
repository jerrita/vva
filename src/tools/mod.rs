pub mod filesystem;
pub mod shell;

use crate::model::{ToolCall, ToolDefinition, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
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

#[derive(Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    /// 注册工作空间默认工具集。
    pub fn with_workspace_defaults(workspace: &std::path::Path) -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(crate::tools::filesystem::ReadTool::new(workspace)));
        registry.register(Arc::new(crate::tools::filesystem::EditTool::new(workspace)));
        registry.register(Arc::new(crate::tools::filesystem::GlobTool::new(workspace)));
        registry.register(Arc::new(crate::tools::filesystem::GrepTool::new(workspace)));
        registry.register(Arc::new(crate::tools::shell::ShellTool::new(workspace)));
        registry
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

    pub fn validate_call(&self, call: &ToolCall) -> Result<(), String> {
        let tool = self
            .get(&call.name)
            .ok_or_else(|| format!("Tool not found: {}", call.name))?;
        validate_arguments(&call.arguments, &tool.parameters())
    }

    pub async fn execute_call(&self, call: &ToolCall) -> ToolResult {
        match self.get(&call.name) {
            Some(tool) => match validate_arguments(&call.arguments, &tool.parameters()) {
                Ok(()) => match tool.execute(&call.arguments).await {
                    Ok(content) => ToolResult::success(&call.id, content),
                    Err(err) => ToolResult::error(&call.id, err),
                },
                Err(err) => ToolResult::error(&call.id, err),
            },
            None => ToolResult::error(&call.id, format!("Tool not found: {}", call.name)),
        }
    }
}

fn validate_arguments(arguments: &str, schema: &Value) -> Result<(), String> {
    let arguments: Value = serde_json::from_str(arguments)
        .map_err(|error| format!("Invalid tool arguments JSON: {error}"))?;
    let object = arguments
        .as_object()
        .ok_or_else(|| "Tool arguments must be a JSON object".to_string())?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "Tool parameter schema must define properties".to_string())?;

    for key in object.keys() {
        if !properties.contains_key(key) {
            return Err(format!("Unexpected parameter: {key}"));
        }
    }
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let required = required
            .as_str()
            .ok_or_else(|| "Tool parameter schema contains a non-string requirement".to_string())?;
        if !object.contains_key(required) {
            return Err(format!("Missing required parameter: {required}"));
        }
    }
    for (name, value) in object {
        let definition = &properties[name];
        let expected = definition
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("Tool parameter schema has no type for {name}"))?;
        let valid_type = match expected {
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.is_i64() || value.is_u64(),
            _ => return Err(format!("Unsupported parameter type {expected} for {name}")),
        };
        if !valid_type {
            return Err(format!("Parameter {name} must be a {expected}"));
        }
        if definition.get("format").and_then(Value::as_str) == Some("path") {
            let path = value.as_str().expect("validated string");
            if path.trim() != path
                || path
                    .chars()
                    .any(|ch| matches!(ch, '\0' | '\n' | '\r' | '{' | '}' | '[' | ']'))
            {
                return Err(format!("Parameter {name} must be a valid file path"));
            }
        }
    }
    Ok(())
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
        registry.register(Arc::new(NamedTool("Shell")));
        registry.register(Arc::new(NamedTool("Read")));
        registry.register(Arc::new(NamedTool("Edit")));

        let names: Vec<_> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();

        assert_eq!(names, ["Edit", "Read", "Shell"]);
    }

    #[tokio::test]
    async fn execute_call_rejects_invalid_parameters_before_execution() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(NamedTool("Read")));
        let call = ToolCall::new("call_1", "Read", r#"{"path":"file.rs","extra":true}"#);

        let result = registry.execute_call(&call).await;

        assert!(result.is_error);
        assert_eq!(result.content, "Unexpected parameter: extra");
    }

    #[test]
    fn validate_call_rejects_malformed_edit_path() {
        let registry = ToolRegistry::with_workspace_defaults(std::path::Path::new("/tmp"));
        let call = ToolCall::new(
            "call_1",
            "Edit",
            r#"{"path":"src/ui/views/chat.rs}]}to=functions.Edit","old_string":"old","new_string":"new"}"#,
        );

        assert_eq!(
            registry.validate_call(&call),
            Err("Parameter path must be a valid file path".to_string())
        );
    }
}
