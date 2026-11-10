//! vva 的技能发现顺序与斜杠调用策略。
use std::path::{Path, PathBuf};
pub use vva_core::skills::{SKILL_FILE, Skill, SkillSource, format_skill_content, parse_skill};

pub fn discover_skills(workspace: &Path) -> Vec<Skill> {
    discover_from(&crate::discover::global_dirs("skills"), workspace)
}

fn discover_from(global_dirs: &[PathBuf], workspace: &Path) -> Vec<Skill> {
    let dirs = global_dirs
        .iter()
        .cloned()
        .map(|dir| (dir, SkillSource::Global))
        .chain(
            crate::discover::project_dirs(workspace, "skills")
                .into_iter()
                .map(|dir| (dir, SkillSource::Project)),
        )
        .collect::<Vec<_>>();
    vva_core::skills::discover_skills(&dirs)
}

/// 解析 /<skill_name> 斜杠调用：prompt 首个空白分隔 token 为 "/name" 且精确匹配已发现技能时返回该技能
pub fn match_invocation<'a>(prompt: &str, skills: &'a [Skill]) -> Option<&'a Skill> {
    let token = prompt.split_whitespace().next()?;
    let name = token.strip_prefix('/')?;
    if name.is_empty() {
        return None;
    }
    skills.iter().find(|skill| skill.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write_skill(root: &Path, name: &str, content: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SKILL_FILE), content).unwrap();
        dir
    }

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vva-skills-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn project_overrides_global_on_name_conflict() {
        let global = temp_root("global");
        let workspace = temp_root("project");
        write_skill(
            &global,
            "shared",
            "---\nname: shared\ndescription: Global version.\n---\nGlobal body\n",
        );
        let agent_skills = workspace.join(".agents").join("skills");
        let vva_skills = workspace.join(".vva").join("skills");
        write_skill(
            &agent_skills,
            "shared",
            "---\nname: shared\ndescription: Agent version.\n---\nAgent body\n",
        );
        write_skill(
            &agent_skills,
            "agent-only",
            "---\nname: agent-only\ndescription: From .agents dir.\n---\n",
        );
        write_skill(
            &vva_skills,
            "shared",
            "---\nname: shared\ndescription: Project version.\n---\nProject body\n",
        );

        let skills = discover_from(std::slice::from_ref(&global), &workspace);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "agent-only");
        // 同名覆盖：.vva > .agents > 全局
        assert_eq!(skills[1].name, "shared");
        assert_eq!(skills[1].description, "Project version.");
        assert_eq!(skills[1].source, SkillSource::Project);
        assert!(skills[1].path.ends_with(".vva/skills/shared"));

        let _ = std::fs::remove_dir_all(global);
        let _ = std::fs::remove_dir_all(workspace);
    }

    fn skill_named(name: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: format!("{name} description"),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            path: PathBuf::from(format!("/tmp/skills/{name}")),
            source: SkillSource::Global,
            body: "body".into(),
        }
    }

    #[test]
    fn matches_slash_invocation_exactly() {
        let skills = vec![skill_named("pdf-tools"), skill_named("web")];
        assert_eq!(
            match_invocation("/pdf-tools extract text", &skills)
                .unwrap()
                .name,
            "pdf-tools"
        );
        assert_eq!(match_invocation("/web", &skills).unwrap().name, "web");

        // 首个 token 非精确技能名、未知技能、非斜杠开头均不命中
        assert!(match_invocation("/pdf-tools, please", &skills).is_none());
        assert!(match_invocation("/unknown task", &skills).is_none());
        assert!(match_invocation("plain prompt", &skills).is_none());
        assert!(match_invocation("/", &skills).is_none());
        assert!(match_invocation("", &skills).is_none());
    }
}
