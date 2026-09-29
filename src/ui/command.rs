#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

/// 命令补全候选：内置斜杠命令或已发现的 Agent Skill（/<skill_name> 直接加载）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionCandidate {
    Command(&'static SlashCommand),
    Skill { name: String, description: String },
}

impl CompletionCandidate {
    /// 补全输入框的斜杠 token（含前导 /）
    pub fn token(&self) -> String {
        match self {
            Self::Command(cmd) => cmd.name.to_string(),
            Self::Skill { name, .. } => format!("/{name}"),
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Command(cmd) => cmd.description,
            Self::Skill { description, .. } => description,
        }
    }
}

use ratatui::style::Style;
use ratatui::text::{Line, Span};

pub static COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/sessions",
        aliases: &["/session"],
        description: "Select and resume a session (per project)",
    },
    SlashCommand {
        name: "/skills",
        aliases: &["/skill"],
        description: "Browse discovered Agent Skills (SKILL.md)",
    },
    SlashCommand {
        name: "/new",
        aliases: &[],
        description: "Start a new session with the current configuration",
    },
    SlashCommand {
        name: "/model",
        aliases: &["/m"],
        description: "Select model interactively or switch (/model <id>)",
    },
    SlashCommand {
        name: "/variant",
        aliases: &["/v", "/effort"],
        description: "Select reasoning variant interactively or switch (/variant <effort>)",
    },
    SlashCommand {
        name: "/config",
        aliases: &["/settings"],
        description: "View and adjust session configuration interactively",
    },
    SlashCommand {
        name: "/search",
        aliases: &["/web"],
        description: "Toggle native web search grounding",
    },
    SlashCommand {
        name: "/thinking",
        aliases: &["/think", "/th"],
        description: "Toggle thinking display between concise (one line) and full",
    },
    SlashCommand {
        name: "/clear",
        aliases: &["/c"],
        description: "Delete current session and start a new one",
    },
    SlashCommand {
        name: "/sidebar",
        aliases: &["/sb"],
        description: "Toggle the right sidebar details panel",
    },
    SlashCommand {
        name: "/help",
        aliases: &["/h", "/?"],
        description: "Show command and keyboard shortcuts help",
    },
    SlashCommand {
        name: "/quit",
        aliases: &["/exit", "/q"],
        description: "Exit vva application",
    },
];

/// 匹配当前输入前缀的可用补全候选（内置命令在前、技能在后；仅在输入命令名且尚未输入参数空格时触发）
pub fn match_candidates(input: &str, skills: &[crate::skills::Skill]) -> Vec<CompletionCandidate> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }

    // 若已经输入了空格或换行，说明正在输入参数或多行内容，不再显示候选选单
    if trimmed.contains(' ') || trimmed.contains('\n') {
        return Vec::new();
    }

    let lower_token = trimmed.to_lowercase();

    let mut candidates: Vec<CompletionCandidate> = COMMANDS
        .iter()
        .filter(|cmd| {
            cmd.name.starts_with(&lower_token)
                || cmd.aliases.iter().any(|a| a.starts_with(&lower_token))
        })
        .map(CompletionCandidate::Command)
        .collect();

    candidates.extend(skills.iter().filter_map(|skill| {
        let slash_name = format!("/{}", skill.name);
        slash_name
            .to_lowercase()
            .starts_with(&lower_token)
            .then(|| CompletionCandidate::Skill {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
    }));

    candidates
}

/// 命令列表行：`prefix + 名称(定宽) + 描述`，帮助弹窗与命令浮层共用
pub fn command_row(
    name: &str,
    description: &str,
    prefix: &str,
    name_width: usize,
    name_style: Style,
    desc_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{prefix}{:<name_width$}", name), name_style),
        Span::styled(format!(" - {description}"), desc_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{Skill, SkillSource};
    use std::path::PathBuf;

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            path: PathBuf::from(format!("/tmp/skills/{name}")),
            source: SkillSource::Global,
            body: String::new(),
        }
    }

    fn names(candidates: &[CompletionCandidate]) -> Vec<String> {
        candidates.iter().map(|c| c.token()).collect()
    }

    #[test]
    fn test_match_candidates() {
        let skills = vec![
            skill("search-docs", "Search docs"),
            skill("pdf", "Pdf tools"),
        ];
        assert_eq!(match_candidates("hello", &skills).len(), 0);

        let s_matches = match_candidates("/s", &skills);
        assert!(names(&s_matches).contains(&"/sessions".to_string()));
        assert!(names(&s_matches).contains(&"/search".to_string()));
        assert!(names(&s_matches).contains(&"/search-docs".to_string()));

        // 内置命令在前，技能在后
        let all_matches = match_candidates("/", &skills);
        assert!(matches!(
            all_matches.first(),
            Some(CompletionCandidate::Command(_))
        ));
        assert!(names(&all_matches).contains(&"/pdf".to_string()));

        let sess_matches = match_candidates("/session", &skills);
        assert_eq!(sess_matches.len(), 1);
        assert_eq!(sess_matches[0].token(), "/sessions");

        let new_matches = match_candidates("/new", &skills);
        assert_eq!(new_matches.len(), 1);
        assert_eq!(new_matches[0].token(), "/new");

        let q_matches = match_candidates("/q", &skills);
        assert_eq!(q_matches.len(), 1);
        assert_eq!(q_matches[0].token(), "/quit");

        let v_matches = match_candidates("/v", &skills);
        assert_eq!(v_matches.len(), 1);
        assert_eq!(v_matches[0].token(), "/variant");

        let cfg_matches = match_candidates("/conf", &skills);
        assert_eq!(cfg_matches.len(), 1);
        assert_eq!(cfg_matches[0].token(), "/config");

        let think_matches = match_candidates("/think", &skills);
        assert_eq!(think_matches.len(), 1);
        assert_eq!(think_matches[0].token(), "/thinking");

        let pdf_matches = match_candidates("/pdf", &skills);
        assert_eq!(pdf_matches.len(), 1);
        assert_eq!(pdf_matches[0].token(), "/pdf");
        assert_eq!(pdf_matches[0].description(), "Pdf tools");

        // 带空格或参数时不显示候选选单
        assert_eq!(match_candidates("/model ", &skills).len(), 0);
        assert_eq!(match_candidates("/model gpt-5", &skills).len(), 0);

        // 多行输入（如粘贴脚本或以 / 开头的多行文本）不触发候选选单
        assert_eq!(match_candidates("/bin/bash\necho 1", &skills).len(), 0);
        assert_eq!(match_candidates("/model\ngpt-5", &skills).len(), 0);
    }
}
