pub mod filesystem;
pub mod shell;

use crate::model::{ToolCall, ToolDefinition, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const MAX_INLINE_OUTPUT_BYTES: usize = 32 * 1024;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;
    async fn execute(&self, arguments: &str) -> Result<String, String>;

    async fn execute_result(&self, call: &ToolCall) -> ToolResult {
        match self.execute(&call.arguments).await {
            Ok(content) => ToolResult::success(&call.id, content),
            Err(error) => ToolResult::error(&call.id, error),
        }
    }

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
    tool_outputs: Option<PathBuf>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            tool_outputs: None,
        }
    }

    /// 注册工作空间默认工具集。
    pub fn with_workspace_defaults(workspace: &Path, tool_outputs: Option<PathBuf>) -> Self {
        let mut registry = Self::new();
        registry.tool_outputs = tool_outputs;
        registry.register(Arc::new(crate::tools::filesystem::ReadTool::new(workspace)));
        registry.register(Arc::new(crate::tools::filesystem::WriteTool::new(
            workspace,
        )));
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
        let Some(tool) = self.get(&call.name) else {
            return ToolResult::error(&call.id, format!("Tool not found: {}", call.name));
        };
        if let Err(err) = validate_arguments(&call.arguments, &tool.parameters()) {
            return ToolResult::error(&call.id, err);
        }

        let mut result = tool.execute_result(call).await;
        if tool.name() != "Read" || result.is_error {
            result.content = match inline_or_spill(
                self.tool_outputs.as_deref(),
                tool.name(),
                result.content,
                result.is_error,
            ) {
                Ok(content) => content,
                Err(error) => return ToolResult::error(&call.id, error),
            };
        }
        result
    }
}

fn inline_or_spill(
    dir: Option<&Path>,
    tool: &str,
    content: String,
    is_error: bool,
) -> Result<String, String> {
    let Some(dir) = dir else {
        return Ok(content);
    };
    if content.len() <= MAX_INLINE_OUTPUT_BYTES {
        return Ok(content);
    }
    std::fs::create_dir_all(dir).map_err(|error| {
        format!(
            "Failed to create tool output directory {}: {error}",
            dir.display()
        )
    })?;
    let path = dir.join(format!("{tool}_{}.txt", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, &content)
        .map_err(|error| format!("Failed to save tool output {}: {error}", path.display()))?;
    let kind = if is_error { "Error output" } else { "Output" };
    Ok(format!(
        "{kind} too large ({} bytes; limit {MAX_INLINE_OUTPUT_BYTES}). Full output saved to {}. Use Read or Grep on that path.",
        content.len(),
        path.display()
    ))
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
        let registry = ToolRegistry::with_workspace_defaults(std::path::Path::new("/tmp"), None);
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

    struct BlobTool {
        error: bool,
    }

    #[async_trait]
    impl Tool for BlobTool {
        fn name(&self) -> &str {
            "Dump"
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn execute(&self, _arguments: &str) -> Result<String, String> {
            let blob = "x".repeat(MAX_INLINE_OUTPUT_BYTES + 1);
            if self.error { Err(blob) } else { Ok(blob) }
        }
    }

    #[tokio::test]
    async fn execute_call_spills_oversize_output() {
        for error in [false, true] {
            let dir =
                std::env::temp_dir().join(format!("vva-tool-outputs-{}", uuid::Uuid::new_v4()));
            let mut registry = ToolRegistry::new();
            registry.tool_outputs = Some(dir.clone());
            registry.register(Arc::new(BlobTool { error }));

            let result = registry
                .execute_call(&ToolCall::new("call_1", "Dump", "{}"))
                .await;

            assert_eq!(result.is_error, error);
            assert!(result.content.contains(&dir.display().to_string()));
            assert!(result.content.contains("Use Read or Grep on that path."));
            let kind = if error { "Error output" } else { "Output" };
            assert!(result.content.starts_with(kind));
            let saved = std::fs::read_dir(&dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            assert_eq!(
                std::fs::read_to_string(&saved).unwrap().len(),
                MAX_INLINE_OUTPUT_BYTES + 1
            );

            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
