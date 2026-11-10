use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::parse_args;
use crate::skills::Skill;

/// Skill 工具：渐进披露第二层，按 name 加载技能的 SKILL.md 全文指令。
/// 技能清单（name + description）由系统提示注入，任务匹配时模型调用本工具。
pub struct SkillTool {
    skills: Arc<Vec<Skill>>,
    description: String,
}

impl SkillTool {
    pub fn new(skills: Arc<Vec<Skill>>, description: String) -> Self {
        Self {
            skills,
            description,
        }
    }
}

#[async_trait]
impl crate::tools::Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input(&self) -> crate::model::ToolInput {
        crate::model::ToolInput::Json({
            let mut name = serde_json::json!({
                "type": "string",
                "description": "Name of the skill to load"
            });
            let names: Vec<&str> = self
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect();
            if !names.is_empty() {
                name["enum"] = serde_json::json!(names);
            }
            serde_json::json!({
                "type": "object",
                "properties": { "name": name },
                "required": ["name"]
            })
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Arguments {
            name: String,
        }
        let args: Arguments = parse_args(arguments, "Skill")?;
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.name == args.name)
            .ok_or_else(|| {
                let available = self
                    .skills
                    .iter()
                    .map(|skill| skill.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if available.is_empty() {
                    "Skill not found: no skills are available".to_string()
                } else {
                    format!(
                        "Skill not found: {}. Available skills: {available}",
                        args.name
                    )
                }
            })?;
        Ok(crate::skills::format_skill_content(skill))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use std::path::PathBuf;

    fn skill(name: &str, body: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: format!("{name} description"),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            path: PathBuf::from(format!("/tmp/skills/{name}")),
            source: crate::skills::SkillSource::Global,
            body: body.to_string(),
        }
    }

    #[tokio::test]
    async fn loads_skill_body_by_name() {
        let tool = SkillTool::new(
            Arc::new(vec![skill("pdf-processing", "1. Open it.")]),
            "Load skill".into(),
        );
        let result = tool.execute(r#"{"name":"pdf-processing"}"#).await.unwrap();
        assert!(result.contains("/tmp/skills/pdf-processing"));
        assert!(result.contains("1. Open it."));
    }

    #[tokio::test]
    async fn unknown_name_reports_available_skills() {
        let tool = SkillTool::new(Arc::new(vec![skill("alpha", "a")]), "Load skill".into());
        let error = tool.execute(r#"{"name":"beta"}"#).await.unwrap_err();
        assert!(error.contains("beta") && error.contains("alpha"), "{error}");
    }
}
