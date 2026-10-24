//! IM 斜杠命令解析与帮助文本。

use super::markdown;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 带文字强制新建会话
    New(String),
    Switch(String),
    Sessions,
    Stop,
    Status,
    SetHome(String),
    UnsetHome(String),
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

impl Command {
    /// 日志用命令名
    pub fn name(&self) -> &'static str {
        match self {
            Command::New(_) => "/new",
            Command::Switch(_) => "/switch",
            Command::Sessions => "/sessions",
            Command::Stop => "/stop",
            Command::Status => "/status",
            Command::SetHome(_) => "/sethome",
            Command::UnsetHome(_) => "/unsethome",
            Command::Rename(_) => "/rename",
            Command::Model(_) => "/model",
            Command::Skills => "/skills",
            Command::Schedule(_) => "/schedule",
            Command::Delete(_) => "/delete",
            Command::Clear(_) => "/clear",
            Command::Help => "/help",
            Command::Unknown(_) => "/unknown",
        }
    }

    pub fn allowed_in_group(&self) -> bool {
        matches!(
            self,
            Self::New(_)
                | Self::Switch(_)
                | Self::Sessions
                | Self::Stop
                | Self::Status
                | Self::Rename(_)
                | Self::Clear(_)
                | Self::SetHome(_)
                | Self::UnsetHome(_)
                | Self::Help
                | Self::Unknown(_)
        )
    }
}

const HELP: &[(&str, &str)] = &[
    ("/new <消息>", "强制新建会话并处理消息"),
    ("/switch <前缀>", "切换默认会话"),
    ("/sessions", "列出会话"),
    ("/stop", "中止默认会话当前轮次"),
    ("/status", "默认会话状态（/stat）"),
    ("/sethome", "将当前聊天加入后台通知接收集合"),
    ("/unsethome", "将当前聊天移出后台通知接收集合"),
    ("/rename <标题>", "重命名默认会话"),
    ("/model", "查看当前模型与推理档位"),
    ("/model list", "列出可用模型与档位"),
    (
        "/model <规格> [档位]",
        "切换默认会话模型；无默认会话时修改新会话模型",
    ),
    ("/model --variant <档位>", "切换推理档位；none 清除档位"),
    (
        "/model --global <规格> [档位]",
        "同时修改默认会话与新会话模型",
    ),
    ("/skills", "列出本地技能"),
    ("/schedule [list]", "查看定时任务；创建任务请直接告诉 agent"),
    ("/schedule pause <id>", "暂停定时任务"),
    ("/schedule resume <id>", "恢复定时任务"),
    ("/schedule delete <id>", "删除定时任务"),
    ("/schedule run <id>", "立即执行一次定时任务"),
    ("/delete [前缀]", "整理记忆后删除会话"),
    ("/clear [前缀]", "直接删除会话，跳过记忆整理"),
    ("/help", "显示本帮助"),
];

pub fn help(group: bool) -> String {
    let mut out = markdown::table(
        "📖 命令帮助",
        ["命令", "说明"],
        HELP.iter()
            .filter(|(usage, _)| !group || parse(usage).is_some_and(|cmd| cmd.allowed_in_group()))
            .map(|(usage, desc)| [usage.to_string(), desc.to_string()]),
    );
    out.push_str(if group {
        "\n\n💡 会话 ID 可使用唯一前缀。`/clear` 省略前缀时操作本群默认会话。"
    } else {
        "\n\n💡 会话与任务 ID 均可使用唯一前缀。`/delete`、`/clear` 省略前缀时操作默认会话。"
    });
    out
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
        "sethome" => Command::SetHome(arg.to_string()),
        "unsethome" => Command::UnsetHome(arg.to_string()),
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
    fn parses_name_and_argument_edges() {
        assert_eq!(
            parse("  /switch  abc1 "),
            Some(Command::Switch("abc1".into()))
        );
        assert_eq!(parse("/Switch@bot ab"), Some(Command::Switch("ab".into())));
        assert_eq!(parse("/stat"), Some(Command::Status));
        assert_eq!(parse("/delete"), Some(Command::Delete(String::new())));
        assert_eq!(
            parse("/model --global p/m high"),
            Some(Command::Model("--global p/m high".into()))
        );
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
