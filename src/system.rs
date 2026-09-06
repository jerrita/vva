use std::path::Path;

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
            "You are vva, an expert AI coding harness assistant.\n\n\
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
