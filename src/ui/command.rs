#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub example: &'static str,
}

pub static COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/sessions",
        aliases: &["/session"],
        description: "Select and resume a session (per project)",
        example: "/sessions",
    },
    SlashCommand {
        name: "/model",
        aliases: &["/m"],
        description: "Select model interactively or switch (/model <id>)",
        example: "/model [model_id]",
    },
    SlashCommand {
        name: "/variant",
        aliases: &["/v", "/effort"],
        description: "Select reasoning variant interactively or switch (/variant <effort>)",
        example: "/variant [effort]",
    },
    SlashCommand {
        name: "/config",
        aliases: &["/settings"],
        description: "View and adjust session configuration interactively",
        example: "/config",
    },
    SlashCommand {
        name: "/search",
        aliases: &["/web"],
        description: "Toggle native web search grounding",
        example: "/search [on|off]",
    },
    SlashCommand {
        name: "/clear",
        aliases: &["/c"],
        description: "Clear message scrollback view",
        example: "/clear",
    },
    SlashCommand {
        name: "/help",
        aliases: &["/h", "/?"],
        description: "Show command and keyboard shortcuts help",
        example: "/help",
    },
    SlashCommand {
        name: "/quit",
        aliases: &["/exit", "/q"],
        description: "Exit vva application",
        example: "/quit",
    },
];

/// 匹配当前输入前缀的可用斜杠命令（仅在输入命令名且尚未输入参数空格时触发）
pub fn match_commands(input: &str) -> Vec<&'static SlashCommand> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }

    // 若已经输入了空格，说明正在输入参数，不再显示命令选单
    if trimmed.contains(' ') {
        return Vec::new();
    }

    let lower_token = trimmed.to_lowercase();

    COMMANDS
        .iter()
        .filter(|cmd| {
            cmd.name.starts_with(&lower_token)
                || cmd.aliases.iter().any(|a| a.starts_with(&lower_token))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_commands() {
        assert_eq!(match_commands("hello").len(), 0);
        let s_matches = match_commands("/s");
        assert!(s_matches.iter().any(|c| c.name == "/sessions"));
        assert!(s_matches.iter().any(|c| c.name == "/search"));

        let sess_matches = match_commands("/session");
        assert_eq!(sess_matches.len(), 1);
        assert_eq!(sess_matches[0].name, "/sessions");

        let q_matches = match_commands("/q");
        assert_eq!(q_matches.len(), 1);
        assert_eq!(q_matches[0].name, "/quit");

        let v_matches = match_commands("/v");
        assert_eq!(v_matches.len(), 1);
        assert_eq!(v_matches[0].name, "/variant");

        let cfg_matches = match_commands("/conf");
        assert_eq!(cfg_matches.len(), 1);
        assert_eq!(cfg_matches[0].name, "/config");

        // 带空格或参数时不显示命令选单
        assert_eq!(match_commands("/model ").len(), 0);
        assert_eq!(match_commands("/model gpt-5").len(), 0);
    }
}

