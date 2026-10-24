//! Subagent 定义：YAML agent 类型（`.vva/agents/*.yaml`），内置 general / explore。
//!
//! 定义决定子代理会话的模型覆写、工具白名单与追加系统提示；
//! 发现优先级与 skills 同构：项目 .vva/agents > 项目 .agents/agents >
//! 全局 ~/.vva/agents > 全局 ~/.agents/agents > 内置，同名高优先级覆盖。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::tools::names::{self, FILE_READ_TOOLS, FILE_TOOLS};

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

fn tool_names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// 内置 general：全工具子代理（不含 Agent 自身与 AskUser，经 YAML 显式开启）
fn builtin_general() -> AgentDef {
    AgentDef {
        name: "general".into(),
        description: "General-purpose agent for generic, multi-step tasks.".into(),
        model: None,
        tools: tool_names(FILE_TOOLS)
            .into_iter()
            .chain(tool_names(&[
                names::SHELL,
                "Skill",
                "TaskOutput",
                "TaskStop",
                "SendMessage",
                "WebSearch",
                "WebFetch",
            ]))
            .collect(),
        system_prompt: Some(
            "You are a general-purpose subagent. Your prompt is self-contained: the parent \
             session does not see your intermediate steps, so your final message must carry \
             every conclusion, file path and fact the caller needs."
                .into(),
        ),
    }
}

/// 内置 explore：只读检索子代理
fn builtin_explore() -> AgentDef {
    AgentDef {
        name: "explore".into(),
        description: "Read-only search agent for locating code and answering questions.".into(),
        model: None,
        tools: tool_names(FILE_READ_TOOLS),
        system_prompt: Some(
            "You are a read-only exploration subagent. Search broadly, read excerpts rather \
             than whole files, and report conclusions with file paths and line references."
                .into(),
        ),
    }
}

/// 内置定义；curator 仅供 agent space 系统创建的会话使用（不暴露给 Agent 工具）
pub fn builtin_agents() -> Vec<AgentDef> {
    vec![
        crate::space::curator::curator_agent(),
        builtin_explore(),
        builtin_general(),
    ]
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
pub fn parse_agent_file(path: &Path) -> Result<AgentDef, String> {
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

/// 发现内置与文件系统定义：按 name 排序返回，同名时后扫描的目录覆盖先扫描的
/// （优先级：项目 .vva > 项目 .agents > 全局 .vva > 全局 .agents > 内置）；
/// 非法定义文件记录 warn 后跳过。
pub fn discover_agents(workspace: &Path) -> Vec<AgentDef> {
    discover_from(&crate::discover::global_dirs("agents"), workspace)
}

/// 指定全局定义目录的发现逻辑（测试注入用）
fn discover_from(global_dirs: &[PathBuf], workspace: &Path) -> Vec<AgentDef> {
    let mut by_name: BTreeMap<String, AgentDef> = builtin_agents()
        .into_iter()
        .map(|def| (def.name.clone(), def))
        .collect();
    let project_dirs = crate::discover::project_dirs(workspace, "agents");
    for dir in global_dirs.iter().chain(&project_dirs) {
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
        assert_eq!(def.tools, ["Read", "Grep"]);
        assert!(def.model.is_none());
        assert!(def.system_prompt.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_optional_fields() {
        let root = temp_root("optional");
        write_def(
            &root,
            "research",
            "description: Research agent.\nmodel: openai/gpt-5.6-terra\ntools: [Read]\nsystem_prompt: |\n  Be thorough.\n",
        );
        let def = parse_agent_file(&root.join("research.yaml")).unwrap();
        assert_eq!(def.model.as_deref(), Some("openai/gpt-5.6-terra"));
        assert_eq!(def.system_prompt.as_deref(), Some("Be thorough.\n"));
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

    #[test]
    fn project_overrides_global_and_builtin() {
        let global_root = temp_root("global");
        let global = global_root.join(".vva").join("agents");
        let workspace = temp_root("project");
        // 同名覆盖内置 general；项目新增自定义 def；项目 .vva 覆盖项目 .agents
        write_def(
            &global,
            "general",
            "description: Global override.\ntools: [Read]\n",
        );
        write_def(
            &workspace.join(".agents").join("agents"),
            "custom",
            "description: Agents dir version.\ntools: [Read]\n",
        );
        write_def(
            &workspace.join(".vva").join("agents"),
            "custom",
            "description: Vva dir version.\ntools: [Read]\n",
        );

        let defs = discover_from(std::slice::from_ref(&global), &workspace);
        let names: Vec<_> = defs.iter().map(|def| def.name.as_str()).collect();
        assert_eq!(names, ["curator", "custom", "explore", "general"]);
        let general = defs.iter().find(|def| def.name == "general").unwrap();
        assert_eq!(general.description, "Global override.");
        let custom = defs.iter().find(|def| def.name == "custom").unwrap();
        assert_eq!(custom.description, "Vva dir version.");

        let _ = std::fs::remove_dir_all(global_root);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn invalid_override_disables_lower_priority_definition() {
        let workspace = temp_root("invalid-override");
        write_def(
            &workspace.join(".vva/agents"),
            "general",
            "description: invalid\ntools: []\n",
        );
        assert!(
            !discover_from(&[], &workspace)
                .iter()
                .any(|def| def.name == "general")
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn builtins_cover_expected_toolsets() {
        let defs = builtin_agents();
        let general = defs.iter().find(|def| def.name == "general").unwrap();
        assert!(general.tools.contains(&"Shell".to_string()));
        assert!(!general.tools.contains(&"Agent".to_string()));
        assert!(!general.tools.contains(&"AskUser".to_string()));
        let explore = defs.iter().find(|def| def.name == "explore").unwrap();
        assert_eq!(explore.tools, FILE_READ_TOOLS);
        assert!(
            FILE_TOOLS
                .iter()
                .all(|tool| general.tools.iter().any(|t| t == tool))
        );
    }
}
