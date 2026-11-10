//! vva 的内置子代理与默认发现顺序。
use std::path::{Path, PathBuf};
use vva_core::agents::AgentDef;
use vva_core::tools::names;
use vva_core::tools::names::FILE_READ_TOOLS;
use vva_core::tools::names::FILE_TOOLS;

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

/// 内置 coding 子代理定义。
pub fn builtin_agents() -> Vec<AgentDef> {
    vec![builtin_explore(), builtin_general()]
}

pub fn discover_agents(workspace: &Path) -> Vec<AgentDef> {
    discover_from(&crate::discover::global_dirs("agents"), workspace)
}

fn discover_from(global_dirs: &[PathBuf], workspace: &Path) -> Vec<AgentDef> {
    let dirs: Vec<_> = global_dirs
        .iter()
        .cloned()
        .chain(crate::discover::project_dirs(workspace, "agents"))
        .collect();
    vva_core::agents::discover_agents(&dirs, builtin_agents())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vva-agents-{tag}-{}", uuid::Uuid::new_v4()))
    }

    fn write_def(root: &Path, stem: &str, content: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(format!("{stem}.yaml")), content).unwrap();
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
        assert_eq!(names, ["custom", "explore", "general"]);
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
}
