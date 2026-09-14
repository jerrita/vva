//! Agent Skills：SKILL.md 技能包的解析与发现（agentskills.io Specification）。
//!
//! 渐进披露三层：发现时仅持有 frontmatter 元数据（name + description 注入系统提示）；
//! 激活时由 Skill 工具返回 SKILL.md 正文；scripts/ 等资源复用既有工具按需读取。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 技能清单文件名（规范必需）
pub const SKILL_FILE: &str = "SKILL.md";

/// 技能来源层级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// 全局 ~/.vva/skills
    Global,
    /// 项目 <workspace>/.vva/skills 与 <workspace>/.agents/skills
    Project,
}

/// 已发现的技能：frontmatter 强类型元数据 + SKILL.md 正文
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: BTreeMap<String, String>,
    /// 实验性字段：仅解析透出，vva 无权限层不强制
    pub allowed_tools: Option<String>,
    /// 技能根目录（SKILL.md 所在目录），资源文件相对此路径
    pub path: PathBuf,
    pub source: SkillSource,
    /// frontmatter 之后的 Markdown 正文
    pub body: String,
}

impl Skill {
    /// 系统提示技能清单行（渐进披露第一层：仅元数据）
    pub fn prompt_line(&self) -> String {
        format!("- {}: {}", self.name, self.description)
    }
}

/// SKILL.md frontmatter 强类型字段集（规范固定字段，未知字段拒绝）
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SkillFrontmatter {
    name: String,
    description: String,
    license: Option<String>,
    compatibility: Option<String>,
    metadata: Option<BTreeMap<String, String>>,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<String>,
}

/// 解析并校验单个技能目录（SKILL.md 缺失或规范校验失败时返回错误）
pub fn parse_skill(dir: &Path, source: SkillSource) -> Result<Skill, String> {
    let dir_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("Invalid skill directory name: {}", dir.display()))?;
    let skill_file = dir.join(SKILL_FILE);
    let content = std::fs::read_to_string(&skill_file)
        .map_err(|error| format!("Failed to read {}: {error}", skill_file.display()))?;
    let (frontmatter, body) = split_frontmatter(&content)?;
    let frontmatter: SkillFrontmatter = serde_yaml::from_str(&frontmatter)
        .map_err(|error| format!("Invalid SKILL.md frontmatter: {error}"))?;

    validate_name(&frontmatter.name, dir_name)?;
    if frontmatter.description.trim().is_empty() {
        return Err(format!(
            "Skill '{}' has an empty description",
            frontmatter.name
        ));
    }
    if frontmatter.description.chars().count() > 1024 {
        return Err(format!(
            "Skill '{}' description exceeds 1024 characters",
            frontmatter.name
        ));
    }
    if frontmatter
        .compatibility
        .as_deref()
        .is_some_and(|text| text.trim().is_empty() || text.chars().count() > 500)
    {
        return Err(format!(
            "Skill '{}' compatibility must be 1-500 characters",
            frontmatter.name
        ));
    }

    Ok(Skill {
        name: frontmatter.name,
        description: frontmatter.description,
        license: frontmatter.license,
        compatibility: frontmatter.compatibility,
        metadata: frontmatter.metadata.unwrap_or_default(),
        allowed_tools: frontmatter.allowed_tools,
        path: dir.to_path_buf(),
        source,
        body,
    })
}

/// 全局技能目录（优先级升序）：~/.agents > ~/.vva
pub fn global_skills_dirs() -> [Option<PathBuf>; 2] {
    match crate::config::home_dir() {
        Some(home) => [
            Some(home.join(".agents").join("skills")),
            Some(home.join(".vva").join("skills")),
        ],
        None => [None, None],
    }
}

/// 项目技能目录（优先级升序）：.vva > .agents
pub fn project_skills_dirs(workspace: &Path) -> [PathBuf; 2] {
    [
        workspace.join(".agents").join("skills"),
        workspace.join(".vva").join("skills"),
    ]
}

/// 发现全局与项目技能：按 name 排序返回，同名时后扫描的目录覆盖先扫描的
/// （优先级：项目 .vva > 项目 .agents > 全局 .vva > 全局 .agents）；非法技能目录记录 warn 后跳过。
pub fn discover_skills(workspace: &Path) -> Vec<Skill> {
    let global_dirs: Vec<PathBuf> = global_skills_dirs().into_iter().flatten().collect();
    discover_from(&global_dirs, workspace)
}

/// 指定全局技能目录的发现逻辑（测试注入用）
fn discover_from(global_dirs: &[PathBuf], workspace: &Path) -> Vec<Skill> {
    let mut by_name = BTreeMap::new();
    for dir in global_dirs {
        collect_skills(dir, SkillSource::Global, &mut by_name);
    }
    for dir in project_skills_dirs(workspace) {
        collect_skills(&dir, SkillSource::Project, &mut by_name);
    }
    by_name.into_values().collect()
}

fn collect_skills(dir: &Path, source: SkillSource, out: &mut BTreeMap<String, Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(SKILL_FILE).is_file() {
            continue;
        }
        match parse_skill(&path, source) {
            Ok(skill) => {
                out.insert(skill.name.clone(), skill);
            }
            Err(error) => tracing::warn!("Skipping invalid skill {}: {error}", path.display()),
        }
    }
}

/// 规范 name 约束：1-64 字符，仅小写字母/数字与非连续连字符，
/// 不得以连字符开头结尾，且必须与父目录名一致
fn validate_name(name: &str, dir_name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.chars().count() <= 64
        && name.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if !valid {
        return Err(format!(
            "Invalid skill name '{name}': must be 1-64 lowercase letters, digits or single hyphens"
        ));
    }
    if name != dir_name {
        return Err(format!(
            "Skill name '{name}' must match its directory name '{dir_name}'"
        ));
    }
    Ok(())
}

/// 切分 `---` 包裹的 YAML frontmatter 与 Markdown 正文
fn split_frontmatter(content: &str) -> Result<(String, String), String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return Err("SKILL.md must begin with a YAML frontmatter block (---)".into());
    }
    let mut frontmatter = String::new();
    let mut body: Option<Vec<&str>> = None;
    for line in lines {
        match &mut body {
            Some(lines) => lines.push(line),
            None => {
                if line == "---" {
                    body = Some(Vec::new());
                } else {
                    frontmatter.push_str(line);
                    frontmatter.push('\n');
                }
            }
        }
    }
    match body {
        Some(lines) => Ok((frontmatter, lines.join("\n"))),
        None => Err("SKILL.md frontmatter is not closed with a --- line".into()),
    }
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
    fn parses_minimal_frontmatter_and_body() {
        let root = temp_root("minimal");
        write_skill(
            &root,
            "pdf-processing",
            "---\nname: pdf-processing\ndescription: Extract PDF text and fill forms.\n---\n\n# Steps\n\n1. Open the file.\n",
        );
        let skill = parse_skill(&root.join("pdf-processing"), SkillSource::Global).unwrap();
        assert_eq!(skill.name, "pdf-processing");
        assert_eq!(skill.description, "Extract PDF text and fill forms.");
        assert!(skill.license.is_none());
        assert!(skill.metadata.is_empty());
        assert_eq!(skill.body, "\n# Steps\n\n1. Open the file.");
        assert_eq!(skill.source, SkillSource::Global);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_optional_fields() {
        let root = temp_root("optional");
        write_skill(
            &root,
            "web-scrape",
            "---\nname: web-scrape\ndescription: Scrape pages.\nlicense: Apache-2.0\ncompatibility: Requires Python 3.12+\nmetadata:\n  author: example-org\n  version: \"1.0\"\nallowed-tools: Bash(curl:*) Read\n---\nBody here.\n",
        );
        let skill = parse_skill(&root.join("web-scrape"), SkillSource::Project).unwrap();
        assert_eq!(skill.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(
            skill.compatibility.as_deref(),
            Some("Requires Python 3.12+")
        );
        assert_eq!(
            skill.metadata.get("author").map(String::as_str),
            Some("example-org")
        );
        assert_eq!(
            skill.metadata.get("version").map(String::as_str),
            Some("1.0")
        );
        assert_eq!(skill.allowed_tools.as_deref(), Some("Bash(curl:*) Read"));
        assert_eq!(skill.source, SkillSource::Project);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_spec_violations() {
        let root = temp_root("invalid");

        // name 含大写 / 连续连字符 / 起始连字符 / 超长
        let too_long = "a".repeat(65);
        for name in [
            "PDF-Processing",
            "pdf--processing",
            "-pdf",
            "pdf_",
            too_long.as_str(),
        ] {
            write_skill(
                &root,
                name.trim_start_matches('-'),
                &format!("---\nname: {name}\ndescription: d\n---\n"),
            );
            assert!(
                parse_skill(
                    &root.join(name.trim_start_matches('-')),
                    SkillSource::Global
                )
                .is_err(),
                "name '{name}' should be rejected"
            );
        }

        // name 与目录名不一致
        write_skill(
            &root,
            "real-dir",
            "---\nname: other-name\ndescription: d\n---\n",
        );
        assert!(parse_skill(&root.join("real-dir"), SkillSource::Global).is_err());

        // description 超长
        write_skill(
            &root,
            "long-desc",
            &format!(
                "---\nname: long-desc\ndescription: {}\n---\n",
                "x".repeat(1025)
            ),
        );
        assert!(parse_skill(&root.join("long-desc"), SkillSource::Global).is_err());

        // compatibility 超长
        write_skill(
            &root,
            "long-compat",
            &format!(
                "---\nname: long-compat\ndescription: d\ncompatibility: {}\n---\n",
                "y".repeat(501)
            ),
        );
        assert!(parse_skill(&root.join("long-compat"), SkillSource::Global).is_err());

        // 未知 frontmatter 字段拒绝
        write_skill(
            &root,
            "unknown-field",
            "---\nname: unknown-field\ndescription: d\ncolor: blue\n---\n",
        );
        assert!(parse_skill(&root.join("unknown-field"), SkillSource::Global).is_err());

        // 缺失 / 未闭合 frontmatter
        write_skill(&root, "no-fm", "# Just markdown\n");
        assert!(parse_skill(&root.join("no-fm"), SkillSource::Global).is_err());
        write_skill(
            &root,
            "unclosed-fm",
            "---\nname: unclosed-fm\ndescription: d\n",
        );
        assert!(parse_skill(&root.join("unclosed-fm"), SkillSource::Global).is_err());

        // 缺 SKILL.md 的子目录静默跳过
        std::fs::create_dir_all(root.join("empty-dir")).unwrap();

        let skills = discover_skills_from_global(&root);
        assert!(skills.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// 测试专用：以指定根目录作为全局技能目录执行发现
    fn discover_skills_from_global(root: &Path) -> Vec<Skill> {
        let mut by_name = BTreeMap::new();
        collect_skills(root, SkillSource::Global, &mut by_name);
        by_name.into_values().collect()
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
}
