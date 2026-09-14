use std::path::{Path, PathBuf};

use crate::skills::Skill;

/// 系统静态环境信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    pub date: String,
    pub os: String,
    pub arch: String,
}

impl SystemInfo {
    /// 采集当前系统的静态环境指标
    pub fn collect() -> Self {
        Self {
            date: get_current_date(),
            os: get_os_info(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    /// 转换为固定 System Prompt 文本
    pub fn to_system_prompt(&self, workspace: &Path, scratch: &Path) -> String {
        format!(
            "You are vva, expert at agentic coding and knowledge work.\n\n\
             ## Environment Context\n\
             - Date: {}\n\
             - Operating System: {} ({})\n\
             - Workspace Root: {}\n\
             - Scratch Pad: {}\n\n\
             The scratch pad is a persistent directory for this project. \
             Read and write files there for durable notes and working artifacts.\n",
            self.date,
            self.os,
            self.arch,
            workspace.display(),
            scratch.display()
        )
    }
}

/// 生成当前工作空间下的固定 System Prompt
pub fn build_system_prompt(workspace: &Path, scratch: &Path) -> String {
    SystemInfo::collect().to_system_prompt(workspace, scratch)
}

/// 格式化已发现技能清单为带有 <system-reminder> 标签的 User Message 文本
/// （对齐 AGENTS.md 注入机制：技能清单不进入 System Prompt）
pub fn format_skills_reminder(skills: &[Skill]) -> String {
    let mut reminder = String::from(
        "<system-reminder type=\"skills\">\n\
         The following skills provide specialized workflows and knowledge for this \
         environment. When a task matches a skill's purpose, load its full instructions \
         with the Skill tool before proceeding.\n\n",
    );
    for skill in skills {
        reminder.push_str(&skill.prompt_line());
        reminder.push('\n');
    }
    reminder.push_str("</system-reminder>");
    reminder
}

/// 格式化 /<skill_name> 直接加载的技能正文为 <system-reminder> User Message 文本
/// （user turn 保留原始输入，技能正文以独立轮次随行注入）
pub fn format_skill_load_reminder(skill: &Skill) -> String {
    format!(
        "<system-reminder type=\"skill-loaded\" name=\"{}\">\n{}\n</system-reminder>",
        skill.name,
        crate::skills::format_skill_content(skill)
    )
}

fn get_current_date() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn run_command_output(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
}

#[cfg(windows)]
fn parse_reg_sz(output: &str, value_name: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(value_name) {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 && parts[1] == "REG_SZ" {
                return Some(parts[2..].join(" "));
            }
        }
    }
    None
}

fn get_os_info() -> String {
    #[cfg(windows)]
    {
        let prod = run_command_output(
            "reg",
            &[
                "query",
                "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
                "/v",
                "ProductName",
            ],
        )
        .and_then(|out| parse_reg_sz(&out, "ProductName"));

        let display_ver = run_command_output(
            "reg",
            &[
                "query",
                "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
                "/v",
                "DisplayVersion",
            ],
        )
        .and_then(|out| parse_reg_sz(&out, "DisplayVersion"));

        let build = run_command_output(
            "reg",
            &[
                "query",
                "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
                "/v",
                "CurrentBuild",
            ],
        )
        .and_then(|out| parse_reg_sz(&out, "CurrentBuild"));

        if let Some(p) = prod {
            let mut result = p;
            if let Some(dv) = display_ver {
                result.push(' ');
                result.push_str(&dv);
            }
            if let Some(b) = build {
                result.push_str(&format!(" (Build {b})"));
            }
            return result;
        }
        format!("Windows ({})", std::env::consts::ARCH)
    }

    #[cfg(target_os = "linux")]
    {
        let distro = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| {
                let mut pretty = None;
                let mut name = None;
                let mut version = None;
                for line in content.lines() {
                    if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
                        pretty = Some(val.trim_matches('"').to_string());
                    } else if let Some(val) = line.strip_prefix("NAME=") {
                        name = Some(val.trim_matches('"').to_string());
                    } else if let Some(val) = line.strip_prefix("VERSION=") {
                        version = Some(val.trim_matches('"').to_string());
                    }
                }
                pretty.or_else(|| match (name, version) {
                    (Some(n), Some(v)) => Some(format!("{n} {v}")),
                    (Some(n), None) => Some(n),
                    _ => None,
                })
            });

        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_string())
            .or_else(|| run_command_output("uname", &["-r"]).map(|s| s.trim().to_string()));

        match (distro, kernel) {
            (Some(d), Some(k)) => format!("{d} (Kernel {k})"),
            (Some(d), None) => d,
            (None, Some(k)) => format!("Linux (Kernel {k})"),
            (None, None) => format!("Linux ({})", std::env::consts::ARCH),
        }
    }

    #[cfg(target_os = "macos")]
    {
        let name =
            run_command_output("sw_vers", &["-productName"]).unwrap_or_else(|| "macOS".into());
        let ver = run_command_output("sw_vers", &["-productVersion"]).unwrap_or_default();
        let build = run_command_output("sw_vers", &["-buildVersion"]).unwrap_or_default();

        let mut res = name.trim().to_string();
        if !ver.trim().is_empty() {
            res.push(' ');
            res.push_str(ver.trim());
        }
        if !build.trim().is_empty() {
            res.push_str(&format!(" (Build {})", build.trim()));
        }
        res
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
    }
}

/// AGENTS.md 系统提示来源类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsReminderKind {
    Global,
    Project,
    Directory,
    /// /<skill_name> 直接加载的技能正文
    SkillLoaded,
}

/// 解析出的 AGENTS.md 系统提示提醒
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
    let path_str = path.to_string_lossy().replace('\\', "/");
    format!(
        "<system-reminder type=\"global\" path=\"~/.vva/AGENTS.md\">\n\
         User instructions from ~/.vva/AGENTS.md ({}):\n\n\
         {}\n\
         </system-reminder>",
        path_str,
        content.trim()
    )
}

/// 格式化项目根目录 AGENTS.md 为带有 <system-reminder> 标签的 User Message 文本
pub fn format_project_agents_reminder(path: &Path, content: &str) -> String {
    let path_str = path.to_string_lossy().replace('\\', "/");
    format!(
        "<system-reminder type=\"project\" path=\"AGENTS.md\">\n\
         Project instructions from {}:\n\n\
         {}\n\
         </system-reminder>",
        path_str,
        content.trim()
    )
}

/// 格式化子目录 AGENTS.md 为带有 <system-reminder> 标签的 User Message 文本
pub fn format_directory_agents_reminder(
    workspace: &Path,
    agents_path: &Path,
    content: &str,
) -> String {
    let ws_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let agents_canonical = agents_path
        .canonicalize()
        .unwrap_or_else(|_| agents_path.to_path_buf());
    let rel_path = agents_canonical
        .strip_prefix(&ws_canonical)
        .unwrap_or(agents_path);
    let rel_str = rel_path.to_string_lossy().replace('\\', "/");

    format!(
        "<system-reminder type=\"directory\" path=\"{}\">\n\
         Directory instructions from {}:\n\n\
         {}\n\
         </system-reminder>",
        rel_str,
        rel_str,
        content.trim()
    )
}

/// 解析文本是否包含 AGENTS.md 的 <system-reminder> 标签
pub fn parse_agents_reminder(text: &str) -> Option<AgentsReminder> {
    let trimmed = text.trim();
    if !trimmed.starts_with("<system-reminder") || !trimmed.ends_with("</system-reminder>") {
        return None;
    }
    let close_tag_idx = trimmed.find('>')?;
    let open_tag = &trimmed[..close_tag_idx];
    let body = trimmed[close_tag_idx + 1..trimmed.len() - "</system-reminder>".len()].trim();

    let kind = if open_tag.contains("type=\"global\"") {
        AgentsReminderKind::Global
    } else if open_tag.contains("type=\"project\"") {
        AgentsReminderKind::Project
    } else if open_tag.contains("type=\"directory\"") {
        AgentsReminderKind::Directory
    } else if open_tag.contains("type=\"skill-loaded\"") {
        AgentsReminderKind::SkillLoaded
    } else {
        return None;
    };

    let path = if let Some(p_idx) = open_tag.find("path=\"") {
        let rest = &open_tag[p_idx + 6..];
        let end_p = rest.find('"')?;
        rest[..end_p].to_string()
    } else {
        String::new()
    };

    let name = if let Some(n_idx) = open_tag.find("name=\"") {
        let rest = &open_tag[n_idx + 6..];
        let end_n = rest.find('"')?;
        rest[..end_n].to_string()
    } else {
        String::new()
    };

    Some(AgentsReminder {
        kind,
        path,
        name,
        content: body.to_string(),
    })
}

/// 读取并校验 AGENTS.md 文件内容（若文件存在且非空）
fn load_agents_file(path: &Path) -> Option<String> {
    if path.is_file() {
        let content = std::fs::read_to_string(path).ok()?;
        if !content.trim().is_empty() {
            return Some(content);
        }
    }
    None
}

/// 加载用户全局 ~/.vva/AGENTS.md（若存在且非空）
pub fn load_global_agents_md() -> Option<(PathBuf, String)> {
    let home = crate::config::home_dir()?;
    let path = home.join(".vva").join("AGENTS.md");
    load_agents_file(&path).map(|c| (path, c))
}

/// 加载项目根目录下的 AGENTS.md（若存在且非空）
pub fn load_project_agents_md(workspace: &Path) -> Option<(PathBuf, String)> {
    let path = workspace.join("AGENTS.md");
    load_agents_file(&path).map(|c| (path, c))
}

/// 向上查找最近的 AGENTS.md（从文件所在目录向上遍历至 workspace 根目录，排除根目录已在初始加载的 AGENTS.md）
pub fn find_directory_agents_md(workspace: &Path, file_path: &Path) -> Option<PathBuf> {
    if file_path.file_name().and_then(|n| n.to_str()) == Some("AGENTS.md") {
        return None;
    }

    let full_path = if file_path.is_relative() {
        workspace.join(file_path)
    } else {
        file_path.to_path_buf()
    };

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
    session.turns.iter().any(|t| {
        t.role == crate::model::Role::User
            && t.messages.iter().any(|m| {
                m.parts.iter().any(|p| match p {
                    crate::model::ContentPart::Text { text, .. } => parse_agents_reminder(text)
                        .is_some_and(|r| r.path.replace('\\', "/") == normalized),
                    _ => false,
                })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn system_prompt_includes_workspace_and_scratch() {
        let info = SystemInfo {
            date: "2026-09-06".into(),
            os: "Linux".into(),
            arch: "x86_64".into(),
        };
        let prompt = info.to_system_prompt(Path::new("/ws"), Path::new("/data/scratch"));
        assert!(prompt.contains("Workspace Root: /ws"));
        assert!(prompt.contains("Scratch Pad: /data/scratch"));
    }

    #[test]
    fn skills_reminder_lists_skills_as_user_message() {
        let skill = crate::skills::Skill {
            name: "pdf-processing".into(),
            description: "Extract PDF text and fill forms.".into(),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            path: PathBuf::from("/skills/pdf-processing"),
            source: crate::skills::SkillSource::Global,
            body: String::new(),
        };
        let reminder = format_skills_reminder(&[skill]);
        assert!(reminder.starts_with("<system-reminder type=\"skills\">"));
        assert!(reminder.ends_with("</system-reminder>"));
        assert!(reminder.contains("- pdf-processing: Extract PDF text and fill forms."));

        // 系统提示不含技能清单
        let prompt = build_system_prompt(Path::new("/ws"), Path::new("/scratch"));
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
    fn format_and_parse_skill_load_reminder() {
        let skill = crate::skills::Skill {
            name: "pdf-processing".into(),
            description: "Extract PDF text.".into(),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            path: PathBuf::from("/skills/pdf-processing"),
            source: crate::skills::SkillSource::Global,
            body: "1. Open the file.".into(),
        };
        let reminder = format_skill_load_reminder(&skill);
        assert!(
            reminder.starts_with("<system-reminder type=\"skill-loaded\" name=\"pdf-processing\">")
        );
        assert!(reminder.ends_with("</system-reminder>"));
        assert!(
            reminder.contains("<skill name=\"pdf-processing\" path=\"/skills/pdf-processing\">")
        );
        assert!(reminder.contains("1. Open the file."));

        let parsed = parse_agents_reminder(&reminder).unwrap();
        assert_eq!(parsed.kind, AgentsReminderKind::SkillLoaded);
        assert_eq!(parsed.name, "pdf-processing");
        assert!(parsed.content.contains("<skill name=\"pdf-processing\""));

        // 普通消息与 skills 清单 reminder 不被误判为 skill-loaded
        assert_eq!(parse_agents_reminder("plain prompt"), None);
        let list_reminder = format_skills_reminder(&[skill]);
        assert_eq!(parse_agents_reminder(&list_reminder), None);
    }

    #[test]
    fn test_find_directory_agents_md_hierarchical() {
        let temp = std::env::temp_dir().join(format!("vva_agents_test_{}", uuid::Uuid::new_v4()));
        let ws = temp.join("workspace");
        let src_ui_views = ws.join("src/ui/views");
        fs::create_dir_all(&src_ui_views).unwrap();

        // 根目录 AGENTS.md
        fs::write(ws.join("AGENTS.md"), "root agents").unwrap();
        // 子模块 AGENTS.md
        fs::write(ws.join("src/ui/AGENTS.md"), "ui agents").unwrap();
        // 目标文件
        let target_file = src_ui_views.join("chat.rs");
        fs::write(&target_file, "fn chat() {}").unwrap();

        // 查找 chat.rs：应向上找到最近的 src/ui/AGENTS.md
        let found = find_directory_agents_md(&ws, &target_file);
        assert_eq!(
            found.unwrap().canonicalize().unwrap(),
            ws.join("src/ui/AGENTS.md").canonicalize().unwrap()
        );

        // 如果直接读取 AGENTS.md 自身，不触发加载
        let self_found = find_directory_agents_md(&ws, &ws.join("src/ui/AGENTS.md"));
        assert!(self_found.is_none());

        // 如果在根目录下读取文件，不重复加载根目录 AGENTS.md
        let root_file = ws.join("main.rs");
        fs::write(&root_file, "fn main() {}").unwrap();
        let root_found = find_directory_agents_md(&ws, &root_file);
        assert!(root_found.is_none());

        let _ = fs::remove_dir_all(temp);
    }
}
