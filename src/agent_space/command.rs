//! IM 斜杠命令解析：`/new /switch /sessions /stop /status /delete`。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 带文字强制新建会话
    New(String),
    Switch(String),
    Sessions,
    Stop,
    Status,
    Delete,
}

/// 非斜杠命令（含未知 `/xxx`）返回 `None`，按普通消息处理
pub fn parse(text: &str) -> Option<Command> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, arg.trim()),
        None => (rest, ""),
    };
    Some(match name {
        "new" => Command::New(arg.to_string()),
        "switch" => Command::Switch(arg.to_string()),
        "sessions" => Command::Sessions,
        "stop" => Command::Stop,
        "status" => Command::Status,
        "delete" => Command::Delete,
        _ => return None,
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
        assert_eq!(parse("/delete"), Some(Command::Delete));
    }

    #[test]
    fn unknown_or_plain_text_is_not_a_command() {
        assert_eq!(parse("/unknown x"), None);
        assert_eq!(parse("hello /new"), None);
        assert_eq!(parse("/newfoo"), None);
    }
}
