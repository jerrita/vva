use std::path::{Path, PathBuf};

use crate::skills::Skill;

/// 生成当前工作空间下的固定 System Prompt
pub fn build_system_prompt(workspace: &Path, scratch: &Path) -> String {
    format!(
        "You are vva, expert at agentic coding and knowledge work.\n\n\
         ## Environment Context\n\
         - Date: {}\n\
         - Operating System: {} ({})\n\
         - Workspace Root: {}\n\
         - Scratch Pad: {}\n\n\
         The Scratch Pad is the absolute directory shown in Environment Context. \
         Use that absolute path for durable notes and working artifacts. \
         Relative paths including scratch/... resolve under the workspace root, \
         not the Scratch Pad; there is no scratch alias.\n",
        chrono::Local::now().format("%Y-%m-%d"),
        get_os_info(),
        std::env::consts::ARCH,
        workspace.display(),
        scratch.display()
    )
}

/// 包裹 `<system-reminder>` 标签；`attrs` 为开标签内的属性串
fn reminder(attrs: &str, body: &str) -> String {
    format!("<system-reminder {attrs}>\n{body}\n</system-reminder>")
}

/// 路径展示归一：统一使用 `/` 分隔
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// 技能指纹：覆盖 description 与正文，任一变化都视为技能变动
fn skill_hash(skill: &Skill) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(skill.description.as_bytes());
    hasher.update([0]);
    hasher.update(skill.body.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 技能指纹索引：`name:hash,...`（按 name 排序），随清单 / 变动提醒写入历史，作为下次比对的基线
fn skills_index(skills: &[Skill]) -> String {
    let mut entries: Vec<String> = skills
        .iter()
        .map(|skill| format!("{}:{}", skill.name, skill_hash(skill)))
        .collect();
    entries.sort();
    entries.join(",")
}

/// 格式化已发现技能清单为带有 <system-reminder> 标签的 User Message 文本
/// （对齐 AGENTS.md 注入机制：技能清单不进入 System Prompt）
pub fn format_skills_reminder(skills: &[Skill]) -> String {
    let lines: Vec<String> = skills.iter().map(Skill::prompt_line).collect();
    reminder(
        &format!("type=\"skills\" index=\"{}\"", skills_index(skills)),
        &format!(
            "The following skills provide specialized workflows and knowledge for this \
             environment. When a task matches a skill's purpose, load its full instructions \
             with the Skill tool before proceeding.\n\n{}",
            lines.join("\n")
        ),
    )
}

/// 历史中最近一次技能提醒（`skills` / `skills-changed`）所记录的基线
enum SkillsBaseline {
    /// 历史中没有技能提醒：创建时无技能
    Empty,
    /// 旧版提醒不带指纹索引，无法逐项比对
    Unindexed,
    Indexed(std::collections::BTreeMap<String, String>),
}

fn skills_baseline<'a>(history: impl Iterator<Item = &'a str>) -> SkillsBaseline {
    let mut baseline = SkillsBaseline::Empty;
    for text in history {
        let trimmed = text.trim();
        if !trimmed.starts_with("<system-reminder") || !trimmed.ends_with("</system-reminder>") {
            continue;
        }
        let Some(open_end) = trimmed.find('>') else {
            continue;
        };
        let open_tag = &trimmed[..open_end];
        if !matches!(
            tag_attr(open_tag, "type"),
            Some("skills" | "skills-changed")
        ) {
            continue;
        }
        baseline = match tag_attr(open_tag, "index") {
            Some(index) => SkillsBaseline::Indexed(
                index
                    .split(',')
                    .filter_map(|entry| entry.split_once(':'))
                    .map(|(name, hash)| (name.to_string(), hash.to_string()))
                    .collect(),
            ),
            None => SkillsBaseline::Unindexed,
        };
    }
    baseline
}

/// 与会话历史中的基线比对当前技能：有新增 / 更新 / 移除时返回 `skills-changed` 提醒，否则 `None`。
/// `history` 为会话中用户轮次的文本，按时间顺序
pub fn skills_change_reminder<'a>(
    history: impl Iterator<Item = &'a str>,
    skills: &[Skill],
) -> Option<String> {
    let current = skills_index(skills);
    let line = |skill: &Skill| format!("- {}: {}", skill.name, skill.description);
    let (mut added, mut updated, mut removed) = (Vec::new(), Vec::new(), Vec::new());
    let refreshed = match skills_baseline(history) {
        SkillsBaseline::Indexed(known) => {
            for skill in skills {
                match known.get(&skill.name) {
                    None => added.push(line(skill)),
                    Some(old) if *old != skill_hash(skill) => updated.push(line(skill)),
                    Some(_) => {}
                }
            }
            for name in known.keys() {
                if !skills.iter().any(|skill| &skill.name == name) {
                    removed.push(format!("- {name}"));
                }
            }
            false
        }
        SkillsBaseline::Empty => {
            added = skills.iter().map(line).collect();
            false
        }
        SkillsBaseline::Unindexed => {
            added = skills.iter().map(line).collect();
            true
        }
    };
    if added.is_empty() && updated.is_empty() && removed.is_empty() && !refreshed {
        return None;
    }
    let mut body = String::from(if refreshed {
        "The available skills list was refreshed. Load a skill's instructions with the Skill tool \
         when a task matches its purpose."
    } else {
        "The set of available skills changed since the last message. Load a skill's \
         instructions with the Skill tool when a task matches its purpose."
    });
    let sections = [
        (if refreshed { "Available" } else { "Added" }, added),
        ("Updated (description or instructions changed)", updated),
        ("Removed (no longer available)", removed),
    ];
    for (title, lines) in sections {
        if !lines.is_empty() {
            body.push_str(&format!("\n\n{title}:\n{}", lines.join("\n")));
        }
    }
    if refreshed && skills.is_empty() {
        body.push_str("\n\nNo skills are currently available.");
    }
    Some(reminder(
        &format!("type=\"skills-changed\" index=\"{current}\""),
        &body,
    ))
}

/// 格式化 /<skill_name> 直接加载的技能正文为 <system-reminder> User Message 文本
/// （user turn 保留原始输入，技能正文以独立轮次随行注入）
pub fn format_skill_load_reminder(skill: &Skill) -> String {
    reminder(
        &format!("type=\"skill-loaded\" name=\"{}\"", skill.name),
        &crate::skills::format_skill_content(skill),
    )
}

/// 格式化后台任务完成通知为 <system-reminder> User Message 文本
/// （正文首行为 UI 展示的状态行，其余为模型可见的输出详情）
pub fn format_background_task_reminder(notification: &str) -> String {
    reminder("type=\"background-task\"", notification.trim())
}

/// 格式化 rollover 摘要为 <system-reminder> User Message 文本（新会话首条动态内容，
/// 承接 `from` 会话被压缩的历史）
pub fn format_rollover_summary_reminder(from: &str, summary: &str) -> String {
    reminder(
        &format!("type=\"rollover-summary\" from=\"{from}\""),
        &format!(
            "This session continues session {from}, whose earlier history was compressed \
             into the summary below. The most recent turns follow verbatim. Items marked \
             unconfirmed are not verified facts.\n\n{}",
            summary.trim()
        ),
    )
}

/// 格式化记忆索引快照为 <system-reminder> User Message 文本（agent space 会话创建时快照，
/// `dir` 为记忆库绝对路径，主题文件按需 Read）
pub fn format_memory_reminder(dir: &Path, index: &str) -> String {
    reminder(
        "type=\"memory\"",
        &format!(
            "Long-term memory index (snapshot at session creation). Topic files live in \
             {}; Read them when you need the details.\n\n{}",
            slash(dir),
            index.trim()
        ),
    )
}

/// 格式化记忆更新提示：只带 curator 摘要，不重复索引全文
pub fn format_memory_updated_reminder(dir: &Path, summary: &str) -> String {
    reminder(
        "type=\"memory-updated\"",
        &format!(
            "Long-term memory was updated after this session started: {}\n\nRead {}/MEMORY.md \
             for the latest index if relevant.",
            summary.trim(),
            slash(dir)
        ),
    )
}

/// 格式化定时任务触发为 <system-reminder> User Message 文本（非用户即时发送的消息）
pub fn format_scheduled_reminder(id: &str, trigger: &str, prompt: &str) -> String {
    reminder(
        &format!("type=\"scheduled\" task=\"{id}\" trigger=\"{trigger}\""),
        &format!(
            "This is a scheduled trigger, not a message the user just sent. Carry out the \
             task below and reply with the text to deliver to the user. If there is nothing \
             worth telling the user, reply with exactly [SILENT] and nothing else.\n\n{}",
            prompt.trim()
        ),
    )
}

/// 文本是否为 <system-reminder> 注入内容（非用户轮次边界）
pub fn is_reminder_text(text: &str) -> bool {
    text.trim_start().starts_with("<system-reminder")
}

/// 当前平台的 OS 描述（发行版 / 版本 / 内核或构建号）
fn get_os_info() -> String {
    #[cfg(windows)]
    {
        let value = |name: &str| {
            let output = std::process::Command::new("reg")
                .args([
                    "query",
                    "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
                    "/v",
                    name,
                ])
                .output()
                .ok()
                .filter(|o| o.status.success())?;
            let text = String::from_utf8(output.stdout).ok()?;
            text.lines().find_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                (parts.len() >= 3 && parts[0] == name && parts[1] == "REG_SZ")
                    .then(|| parts[2..].join(" "))
            })
        };
        let Some(mut result) = value("ProductName") else {
            return format!("Windows ({})", std::env::consts::ARCH);
        };
        if let Some(version) = value("DisplayVersion") {
            result.push_str(&format!(" {version}"));
        }
        if let Some(build) = value("CurrentBuild") {
            result.push_str(&format!(" (Build {build})"));
        }
        result
    }

    #[cfg(target_os = "linux")]
    {
        let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        let field = |key: &str| {
            release
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .map(|value| value.trim_matches('"').to_string())
        };
        let distro = field("PRETTY_NAME=").or_else(|| match (field("NAME="), field("VERSION=")) {
            (Some(name), Some(version)) => Some(format!("{name} {version}")),
            (name, _) => name,
        });
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_string());

        match (distro, kernel) {
            (Some(d), Some(k)) => format!("{d} (Kernel {k})"),
            (Some(d), None) => d,
            (None, Some(k)) => format!("Linux (Kernel {k})"),
            (None, None) => format!("Linux ({})", std::env::consts::ARCH),
        }
    }

    #[cfg(target_os = "macos")]
    {
        let query = |flag: &str| {
            std::process::Command::new("sw_vers")
                .arg(flag)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
        };
        let mut result = query("-productName").unwrap_or_else(|| "macOS".into());
        if let Some(version) = query("-productVersion") {
            result.push_str(&format!(" {version}"));
        }
        if let Some(build) = query("-buildVersion") {
            result.push_str(&format!(" (Build {build})"));
        }
        result
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
    }
}

/// 系统提醒来源类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsReminderKind {
    Global,
    Project,
    Directory,
    /// /<skill_name> 直接加载的技能正文
    SkillLoaded,
    /// 会话首条消息随行注入的技能清单
    Skills,
    /// 后台任务完成通知（Shell run_in_background）
    BackgroundTask,
}

/// 解析出的系统提醒
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsReminder {
    pub kind: AgentsReminderKind,
    pub path: String,
    /// skill-loaded 时的技能名，其余类别为空
    pub name: String,
    pub content: String,
}

/// 格式化全局 ~/.vva/AGENTS.md 为带有 <system-reminder> 标签的 User Message 文本
pub fn format_global_agents_reminder(path: &Path, content: &str) -> String {
    reminder(
        "type=\"global\" path=\"~/.vva/AGENTS.md\"",
        &format!(
            "User instructions from ~/.vva/AGENTS.md ({}):\n\n{}",
            slash(path),
            content.trim()
        ),
    )
}

/// 格式化项目根目录 AGENTS.md 为带有 <system-reminder> 标签的 User Message 文本
pub fn format_project_agents_reminder(path: &Path, content: &str) -> String {
    reminder(
        "type=\"project\" path=\"AGENTS.md\"",
        &format!(
            "Project instructions from {}:\n\n{}",
            slash(path),
            content.trim()
        ),
    )
}

/// 格式化子目录 AGENTS.md 为带有 <system-reminder> 标签的 User Message 文本
pub fn format_directory_agents_reminder(
    workspace: &Path,
    agents_path: &Path,
    content: &str,
) -> String {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let agents_canonical = canonical(agents_path);
    let rel_path = agents_canonical
        .strip_prefix(canonical(workspace))
        .unwrap_or(agents_path);
    let rel = slash(rel_path);
    reminder(
        &format!("type=\"directory\" path=\"{rel}\""),
        &format!("Directory instructions from {rel}:\n\n{}", content.trim()),
    )
}

/// 取开标签中 `key="..."` 属性值
fn tag_attr<'a>(open_tag: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("{key}=\"");
    let rest = &open_tag[open_tag.find(&marker)? + marker.len()..];
    Some(&rest[..rest.find('"')?])
}

/// 解析文本是否为系统提醒的 <system-reminder> 标签
pub fn parse_agents_reminder(text: &str) -> Option<AgentsReminder> {
    const CLOSE_TAG: &str = "</system-reminder>";
    let trimmed = text.trim();
    if !trimmed.starts_with("<system-reminder") || !trimmed.ends_with(CLOSE_TAG) {
        return None;
    }
    let open_end = trimmed.find('>')?;
    let open_tag = &trimmed[..open_end];
    let body = trimmed[open_end + 1..trimmed.len() - CLOSE_TAG.len()].trim();

    let kind = match tag_attr(open_tag, "type")? {
        "global" => AgentsReminderKind::Global,
        "project" => AgentsReminderKind::Project,
        "directory" => AgentsReminderKind::Directory,
        "skill-loaded" => AgentsReminderKind::SkillLoaded,
        "skills" | "skills-changed" => AgentsReminderKind::Skills,
        "background-task" => AgentsReminderKind::BackgroundTask,
        _ => return None,
    };

    Some(AgentsReminder {
        kind,
        path: tag_attr(open_tag, "path").unwrap_or_default().to_string(),
        name: tag_attr(open_tag, "name").unwrap_or_default().to_string(),
        content: body.to_string(),
    })
}

/// 读取 AGENTS.md 文件内容（文件存在且非空）
fn load_agents_file(path: PathBuf) -> Option<(PathBuf, String)> {
    let content = std::fs::read_to_string(&path).ok()?;
    (!content.trim().is_empty()).then_some((path, content))
}

/// 加载用户全局 ~/.vva/AGENTS.md（若存在且非空）
pub fn load_global_agents_md() -> Option<(PathBuf, String)> {
    load_agents_file(crate::config::home_dir()?.join(".vva").join("AGENTS.md"))
}

/// 加载项目根目录下的 AGENTS.md（若存在且非空）
pub fn load_project_agents_md(workspace: &Path) -> Option<(PathBuf, String)> {
    load_agents_file(workspace.join("AGENTS.md"))
}

/// 向上查找最近的 AGENTS.md（从文件所在目录向上遍历至 workspace 根目录，排除根目录已在初始加载的 AGENTS.md）
pub fn find_directory_agents_md(workspace: &Path, file_path: &Path) -> Option<PathBuf> {
    if file_path.file_name().and_then(|n| n.to_str()) == Some("AGENTS.md") {
        return None;
    }

    let full_path = workspace.join(file_path);
    let ws_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let file_canonical = full_path.canonicalize().unwrap_or(full_path);

    let mut current = file_canonical.parent()?;
    while current.starts_with(&ws_canonical) && current != ws_canonical {
        let candidate = current.join("AGENTS.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = current.parent()?;
    }

    None
}

/// 检查会话轮次中是否已经加载过特定相对路径的 AGENTS.md
pub fn session_contains_agents_md(session: &crate::model::Session, rel_path: &str) -> bool {
    let normalized = rel_path.replace('\\', "/");
    session
        .turns
        .iter()
        .filter(|turn| turn.role == crate::model::Role::User)
        .flat_map(|turn| &turn.messages)
        .flat_map(|message| &message.parts)
        .any(|part| match part {
            crate::model::ContentPart::Text { text, .. } => {
                parse_agents_reminder(text).is_some_and(|r| r.path.replace('\\', "/") == normalized)
            }
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn skill(name: &str, description: &str, body: &str) -> Skill {
        Skill {
            name: name.into(),
            description: description.into(),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            path: PathBuf::from(format!("/skills/{name}")),
            source: crate::skills::SkillSource::Global,
            body: body.into(),
        }
    }

    #[test]
    fn system_prompt_includes_workspace_and_scratch_but_no_skills() {
        let prompt = build_system_prompt(Path::new("/ws"), Path::new("/data/scratch"));
        assert!(prompt.contains("Workspace Root: /ws"));
        assert!(prompt.contains("Scratch Pad: /data/scratch"));
        assert!(!prompt.contains("Skills"));
    }

    #[test]
    fn test_format_and_parse_agents_reminders() {
        let global_rem =
            format_global_agents_reminder(Path::new("/home/user/.vva/AGENTS.md"), "global rules");
        let parsed_global = parse_agents_reminder(&global_rem).unwrap();
        assert_eq!(parsed_global.kind, AgentsReminderKind::Global);
        assert_eq!(parsed_global.path, "~/.vva/AGENTS.md");
        assert!(parsed_global.content.contains("global rules"));

        let project_rem =
            format_project_agents_reminder(Path::new("/repo/AGENTS.md"), "project rules");
        let parsed_proj = parse_agents_reminder(&project_rem).unwrap();
        assert_eq!(parsed_proj.kind, AgentsReminderKind::Project);
        assert_eq!(parsed_proj.path, "AGENTS.md");
        assert!(parsed_proj.content.contains("project rules"));

        let dir_rem = format_directory_agents_reminder(
            Path::new("/repo"),
            Path::new("/repo/src/ui/AGENTS.md"),
            "ui rules",
        );
        let parsed_dir = parse_agents_reminder(&dir_rem).unwrap();
        assert_eq!(parsed_dir.kind, AgentsReminderKind::Directory);
        assert_eq!(parsed_dir.path, "src/ui/AGENTS.md");
        assert!(parsed_dir.content.contains("ui rules"));
    }

    #[test]
    fn skills_change_reminder_diffs_against_history_baseline() {
        let a = skill("alpha", "Alpha skill.", "a body");
        let b = skill("beta", "Beta skill.", "b body");
        let base = format_skills_reminder(&[a.clone(), b.clone()]);
        let history = |texts: &[&str]| texts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        let run = |texts: &[String], now: &[Skill]| {
            skills_change_reminder(texts.iter().map(String::as_str), now)
        };

        // 无变化：不注入
        let h = history(&[&base, "hello"]);
        assert_eq!(run(&h, &[a.clone(), b.clone()]), None);

        // 新增 + 正文更新 + 移除
        let c = skill("gamma", "Gamma skill.", "c body");
        let a2 = skill("alpha", "Alpha skill.", "a body v2");
        let r = run(&h, &[a2.clone(), c.clone()]).unwrap();
        assert!(r.starts_with("<system-reminder type=\"skills-changed\" index=\""));
        assert!(r.contains("Added:\n- gamma: Gamma skill."), "{r}");
        assert!(
            r.contains("Updated (description or instructions changed):\n- alpha"),
            "{r}"
        );
        assert!(r.contains("Removed (no longer available):\n- beta"), "{r}");
        assert_eq!(
            parse_agents_reminder(&r).unwrap().kind,
            AgentsReminderKind::Skills
        );

        // 变动提醒成为新基线：同一状态不重复提示
        let h2 = history(&[&base, &r, "next"]);
        assert_eq!(run(&h2, &[a2, c]), None);

        // 创建时无技能（无提醒）：之后出现的技能按新增提示
        let none: Vec<String> = history(&["hi"]);
        assert!(
            run(&none, std::slice::from_ref(&a))
                .unwrap()
                .contains("Added:")
        );
        assert_eq!(run(&none, &[]), None);

        // 旧版不带指纹的清单：整体刷新一次
        let legacy = history(&[
            "<system-reminder type=\"skills\">\n- alpha: Alpha skill.\n</system-reminder>",
        ]);
        let r = run(&legacy, std::slice::from_ref(&a)).unwrap();
        assert!(
            r.contains("refreshed") && r.contains("Available:\n- alpha"),
            "{r}"
        );
    }

    #[test]
    fn format_and_parse_skill_reminders() {
        let skill = skill("pdf-processing", "Extract PDF text.", "1. Open the file.");

        let list = format_skills_reminder(std::slice::from_ref(&skill));
        assert!(list.starts_with("<system-reminder type=\"skills\" index=\"pdf-processing:"));
        assert!(list.ends_with("</system-reminder>"));
        assert!(list.contains("- pdf-processing: Extract PDF text."));
        assert_eq!(
            parse_agents_reminder(&list).unwrap().kind,
            AgentsReminderKind::Skills
        );

        let loaded = format_skill_load_reminder(&skill);
        assert!(
            loaded.starts_with("<system-reminder type=\"skill-loaded\" name=\"pdf-processing\">")
        );
        assert!(loaded.contains("<skill name=\"pdf-processing\" path=\"/skills/pdf-processing\">"));
        let parsed = parse_agents_reminder(&loaded).unwrap();
        assert_eq!(parsed.kind, AgentsReminderKind::SkillLoaded);
        assert_eq!(parsed.name, "pdf-processing");
        assert!(parsed.content.contains("1. Open the file."));

        assert_eq!(parse_agents_reminder("plain prompt"), None);
    }

    #[test]
    fn format_and_parse_background_task_reminder() {
        let notification =
            "Background task 1 exited with code 0\nintent: watch build\ncaptured output: 4 bytes";
        let reminder = format_background_task_reminder(notification);
        assert!(reminder.starts_with("<system-reminder type=\"background-task\">"));

        let parsed = parse_agents_reminder(&reminder).unwrap();
        assert_eq!(parsed.kind, AgentsReminderKind::BackgroundTask);
        assert_eq!(parsed.content, notification);
    }

    #[test]
    fn test_find_directory_agents_md_hierarchical() {
        let temp = std::env::temp_dir().join(format!("vva_agents_test_{}", uuid::Uuid::new_v4()));
        let ws = temp.join("workspace");
        let src_ui_views = ws.join("src/ui/views");
        fs::create_dir_all(&src_ui_views).unwrap();

        fs::write(ws.join("AGENTS.md"), "root agents").unwrap();
        fs::write(ws.join("src/ui/AGENTS.md"), "ui agents").unwrap();
        let target_file = src_ui_views.join("chat.rs");
        fs::write(&target_file, "fn chat() {}").unwrap();

        // 应向上找到最近的 src/ui/AGENTS.md
        let found = find_directory_agents_md(&ws, &target_file);
        assert_eq!(
            found.unwrap().canonicalize().unwrap(),
            ws.join("src/ui/AGENTS.md").canonicalize().unwrap()
        );

        // AGENTS.md 自身与根目录文件均不触发加载
        assert!(find_directory_agents_md(&ws, &ws.join("src/ui/AGENTS.md")).is_none());
        let root_file = ws.join("main.rs");
        fs::write(&root_file, "fn main() {}").unwrap();
        assert!(find_directory_agents_md(&ws, &root_file).is_none());

        let _ = fs::remove_dir_all(temp);
    }
}
