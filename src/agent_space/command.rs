//! IM 斜杠命令解析：`/new /switch /sessions /stop /status /delete`。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 带文字强制新建会话
    New(String),
    Switch(String),
    Sessions,
    Stop,
    Status,
    Rename(String),
    /// 空参数查看；`list` 列出；`<规格> [档位]` 切换默认会话模型
    Model(String),
    Skills,
    Schedule(String),
    /// 整理记忆后删除；空前缀为默认会话
    Delete(String),
    /// 直接删除，跳过记忆整理
    Clear(String),
    Help,
    /// 未知命令：显式回复可用命令，不交给模型
    Unknown(String),
}

const HELP: &[(&str, &str)] = &[
    ("/new <消息>", "强制新建会话并处理消息"),
    ("/switch <前缀>", "切换默认会话"),
    ("/sessions", "列出会话"),
    ("/stop", "中止默认会话当前轮次"),
    ("/status", "默认会话状态（/stat）"),
    ("/rename <标题>", "重命名默认会话"),
    (
        "/model [list|[--global] <规格> [档位]]",
        "查看 / 列出 / 切换模型：改 focus 会话，无 focus 改 agent 模式，--global 两者都改",
    ),
    ("/skills", "列出本地技能"),
    (
        "/schedule [list|pause|resume|delete|run <id>]",
        "管理定时任务（创建请直接告诉 agent）",
    ),
    ("/delete [前缀]", "整理记忆后删除会话"),
    ("/clear [前缀]", "直接删除会话，跳过记忆整理"),
    ("/help", "显示本帮助"),
];

pub fn help() -> String {
    HELP.iter()
        .map(|(usage, desc)| format!("{usage}  {desc}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 非斜杠开头返回 `None`，按普通消息处理；`/xxx` 一律是命令（命令名大小写不敏感，容忍 `/cmd@bot`）
pub fn parse(text: &str) -> Option<Command> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, arg.trim()),
        None => (rest, ""),
    };
    let name = name.split('@').next().unwrap_or(name).to_lowercase();
    Some(match name.as_str() {
        "new" => Command::New(arg.to_string()),
        "switch" => Command::Switch(arg.to_string()),
        "sessions" => Command::Sessions,
        "stop" => Command::Stop,
        "status" | "stat" => Command::Status,
        "rename" => Command::Rename(arg.to_string()),
        "model" => Command::Model(arg.to_string()),
        "skills" => Command::Skills,
        "schedule" => Command::Schedule(arg.to_string()),
        "delete" => Command::Delete(arg.to_string()),
        "clear" => Command::Clear(arg.to_string()),
        "help" => Command::Help,
        _ => Command::Unknown(name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_commands_with_arguments() {
        assert_eq!(
            parse("/new 写个脚本"),
            Some(Command::New("写个脚本".into()))
        );
        assert_eq!(
            parse("  /switch  abc1 "),
            Some(Command::Switch("abc1".into()))
        );
        assert_eq!(parse("/sessions"), Some(Command::Sessions));
        assert_eq!(parse("/stop"), Some(Command::Stop));
        assert_eq!(parse("/status"), Some(Command::Status));
        assert_eq!(parse("/delete"), Some(Command::Delete(String::new())));
        assert_eq!(parse("/delete ab"), Some(Command::Delete("ab".into())));
        assert_eq!(parse("/stat"), Some(Command::Status));
        assert_eq!(parse("/clear ab"), Some(Command::Clear("ab".into())));
        assert_eq!(
            parse("/model --global p/m high"),
            Some(Command::Model("--global p/m high".into()))
        );
        assert_eq!(parse("/rename 新名"), Some(Command::Rename("新名".into())));
        assert_eq!(parse("/skills"), Some(Command::Skills));
        assert_eq!(
            parse("/schedule list"),
            Some(Command::Schedule("list".into()))
        );
        assert_eq!(parse("/help"), Some(Command::Help));
        assert_eq!(parse("/Switch@bot ab"), Some(Command::Switch("ab".into())));
    }

    #[test]
    fn unknown_slash_is_a_command_plain_text_is_not() {
        assert_eq!(
            parse("/unknown x"),
            Some(Command::Unknown("unknown".into()))
        );
        assert_eq!(parse("/newfoo"), Some(Command::Unknown("newfoo".into())));
        assert_eq!(parse("hello /new"), None);
        assert_eq!(parse("/ 路径"), Some(Command::Unknown(String::new())));
    }
}
