//! Agent 定义文件解析与显式目录扫描。
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// subagent 嵌套深度上限（顶层会话 depth = 0）
pub const MAX_SUBAGENT_DEPTH: u8 = 3;

/// Agent 定义文件扩展名
const AGENT_FILE_EXT: &str = "yaml";

/// 已生效的 subagent 定义
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// 模型规格覆写（缺省继承父会话 current_model）
    pub model: Option<String>,
    /// 工具白名单，包含原生与客户端工具（WebSearch / WebFetch / ImageGen 使用统一权限名）
    pub tools: Vec<String>,
    /// 追加到基础 System Prompt 之后的角色指令
    pub system_prompt: Option<String>,
}

/// YAML 定义文件字段集（未知字段拒绝）
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AgentDefFile {
    /// 缺省取文件名主干
    name: Option<String>,
    description: String,
    model: Option<String>,
    tools: Vec<String>,
    system_prompt: Option<String>,
}

/// 解析单个定义文件：name 缺省取文件主干，显式时必须与主干一致
pub(crate) fn parse_agent_file(path: &Path) -> Result<AgentDef, String> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| format!("Invalid agent definition file name: {}", path.display()))?;
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    let file: AgentDefFile = serde_yaml::from_str(&content)
        .map_err(|error| format!("Invalid agent definition {}: {error}", path.display()))?;
    let name = file.name.unwrap_or_else(|| stem.to_string());
    crate::discover::validate_name(&name, stem, "agent", "file name")?;
    crate::discover::validate_description(&file.description, "agent", &name)?;
    if file.tools.is_empty() {
        return Err(format!("Agent '{name}' must allow at least one tool"));
    }
    if file.tools.iter().any(|tool| tool.trim().is_empty()) {
        return Err(format!(
            "Agent '{name}' has an empty tool name in its allow-list"
        ));
    }
    Ok(AgentDef {
        name,
        description: file.description,
        model: file.model,
        tools: file.tools,
        system_prompt: file.system_prompt,
    })
}

/// 依次扫描宿主提供的目录，后扫描的同名定义覆盖前者；非法覆盖禁用同名定义。
pub fn discover_agents(dirs: &[std::path::PathBuf], initial: Vec<AgentDef>) -> Vec<AgentDef> {
    let mut by_name = initial
        .into_iter()
        .map(|def| (def.name.clone(), def))
        .collect();
    for dir in dirs {
        collect_agents(dir, &mut by_name);
    }
    by_name.into_values().collect()
}

fn collect_agents(dir: &Path, out: &mut BTreeMap<String, AgentDef>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_yaml = path.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case(AGENT_FILE_EXT));
        if !is_yaml {
            continue;
        }
        match parse_agent_file(&path) {
            Ok(def) => {
                out.insert(def.name.clone(), def);
            }
            Err(error) => {
                // 高优先级定义失效时禁用同名代理，避免恢复低优先级的更宽权限。
                if let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) {
                    out.remove(name);
                }
                tracing::warn!("Skipping invalid agent definition: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vva-agents-{tag}-{}", uuid::Uuid::new_v4()))
    }

    fn write_def(root: &Path, stem: &str, content: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(format!("{stem}.{AGENT_FILE_EXT}")), content).unwrap();
    }

    #[test]
    fn parses_minimal_definition_with_stem_name() {
        let root = temp_root("minimal");
        write_def(
            &root,
            "reviewer",
            "description: Reviews code.\ntools: [Read, Grep]\n",
        );
        let def = parse_agent_file(&root.join("reviewer.yaml")).unwrap();
        assert_eq!(def.name, "reviewer");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_invalid_definitions() {
        let root = temp_root("invalid");
        // 未知字段 / 空工具集 / 空 description / name 与文件名不一致
        write_def(
            &root,
            "unknown",
            "description: d\ntools: [Read]\ncolor: blue\n",
        );
        write_def(&root, "empty-tools", "description: d\ntools: []\n");
        write_def(&root, "empty-desc", "description:  \ntools: [Read]\n");
        write_def(
            &root,
            "mismatch",
            "name: other\ndescription: d\ntools: [Read]\n",
        );
        for stem in ["unknown", "empty-tools", "empty-desc", "mismatch"] {
            assert!(
                parse_agent_file(&root.join(format!("{stem}.yaml"))).is_err(),
                "{stem} should be rejected"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
