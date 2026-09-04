use std::path::Path;

/// 系统与硬件静态环境信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    pub date: String,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub gpu: String,
    pub memory: String,
}

impl SystemInfo {
    /// 采集当前系统的静态环境指标
    pub fn collect() -> Self {
        Self {
            date: get_current_date(),
            os: get_os_info(),
            arch: std::env::consts::ARCH.to_string(),
            cpu: get_cpu_info(),
            gpu: get_gpu_info(),
            memory: get_memory_info(),
        }
    }

    /// 转换为固定 System Prompt 文本
    pub fn to_system_prompt(&self, workspace: &Path) -> String {
        format!(
            "You are vva, an expert AI coding harness assistant.\n\n\
             ## Environment Context\n\
             - Date: {}\n\
             - Operating System: {} ({})\n\
             - CPU: {}\n\
             - GPU: {}\n\
             - Memory: {}\n\
             - Workspace Root: {}\n",
            self.date,
            self.os,
            self.arch,
            self.cpu,
            self.gpu,
            self.memory,
            workspace.display()
        )
    }
}

/// 生成当前工作空间下的固定 System Prompt
pub fn build_system_prompt(workspace: &Path) -> String {
    SystemInfo::collect().to_system_prompt(workspace)
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

fn get_cpu_info() -> String {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    #[cfg(windows)]
    {
        let brand = run_command_output(
            "reg",
            &[
                "query",
                "HKLM\\HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0",
                "/v",
                "ProcessorNameString",
            ],
        )
        .and_then(|out| parse_reg_sz(&out, "ProcessorNameString"));

        if let Some(b) = brand {
            let trimmed = b.trim();
            if !trimmed.is_empty() {
                return format!("{trimmed} ({cores} logical cores)");
            }
        }
        if let Ok(ident) = std::env::var("PROCESSOR_IDENTIFIER") {
            let trimmed = ident.trim();
            if !trimmed.is_empty() {
                return format!("{trimmed} ({cores} logical cores)");
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in content.lines() {
                if (line.starts_with("model name") || line.starts_with("Processor"))
                    && let Some((_, model)) = line.split_once(':')
                {
                    let trimmed = model.trim();
                    if !trimmed.is_empty() {
                        return format!("{trimmed} ({cores} logical cores)");
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(out) = run_command_output("sysctl", &["-n", "machdep.cpu.brand_string"]) {
            let trimmed = out.trim();
            if !trimmed.is_empty() {
                return format!("{trimmed} ({cores} logical cores)");
            }
        }
    }

    format!("{} ({} logical cores)", std::env::consts::ARCH, cores)
}

fn get_gpu_info() -> String {
    #[cfg(windows)]
    {
        if let Some(out) = run_command_output(
            "reg",
            &[
                "query",
                "HKLM\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}",
                "/s",
                "/v",
                "DriverDesc",
            ],
        ) {
            let mut gpus = Vec::new();
            for line in out.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("DriverDesc") {
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    if parts.len() >= 3 && parts[1] == "REG_SZ" {
                        let name = parts[2..].join(" ");
                        let lower = name.to_lowercase();
                        if !lower.contains("virtual")
                            && !lower.contains("remote")
                            && !lower.contains("gameviewer")
                            && !lower.contains("mumu")
                            && !lower.contains("vnc")
                            && !lower.contains("rdp")
                            && !lower.contains("citrix")
                            && !lower.contains("parsec")
                            && !lower.contains("teamviewer")
                            && !lower.contains("anydesk")
                            && !lower.contains("iddsampledriver")
                            && !lower.contains("indirect")
                            && !gpus.contains(&name)
                        {
                            gpus.push(name);
                        }
                    }
                }
            }
            if !gpus.is_empty() {
                return gpus.join(", ");
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(out) =
            run_command_output("nvidia-smi", &["--query-gpu=name", "--format=csv,noheader"])
        {
            let gpus: Vec<String> = out
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            if !gpus.is_empty() {
                return gpus.join(", ");
            }
        }

        if let Some(out) = run_command_output("lspci", &[]) {
            let mut gpus = Vec::new();
            for line in out.lines() {
                if (line.contains("VGA compatible controller") || line.contains("3D controller"))
                    && let Some((_, rest)) = line.split_once(':')
                {
                    let trimmed = rest.trim();
                    if !trimmed.is_empty() && !gpus.contains(&trimmed.to_string()) {
                        gpus.push(trimmed.to_string());
                    }
                }
            }
            if !gpus.is_empty() {
                return gpus.join(", ");
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(out) = run_command_output("system_profiler", &["SPDisplaysDataType"]) {
            let mut gpus = Vec::new();
            for line in out.lines() {
                if line.trim().starts_with("Chipset Model:") {
                    if let Some((_, model)) = line.split_once(':') {
                        let trimmed = model.trim();
                        if !trimmed.is_empty() && !gpus.contains(&trimmed.to_string()) {
                            gpus.push(trimmed.to_string());
                        }
                    }
                }
            }
            if !gpus.is_empty() {
                return gpus.join(", ");
            }
        }
    }

    "None detected".to_string()
}

fn get_memory_info() -> String {
    #[cfg(windows)]
    {
        if let Some(out) =
            run_command_output("wmic", &["ComputerSystem", "get", "TotalPhysicalMemory"])
        {
            for line in out.lines() {
                let trimmed = line.trim();
                if let Ok(bytes) = trimmed.parse::<u64>() {
                    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                    return format!("{gb:.1} GB");
                }
            }
        }

        if let Some(out) = run_command_output(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ],
        ) {
            let trimmed = out.trim();
            if let Ok(bytes) = trimmed.parse::<u64>() {
                let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                return format!("{gb:.1} GB");
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                if line.starts_with("MemTotal:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2
                        && let Ok(kb) = parts[1].parse::<f64>()
                    {
                        let gb = kb / (1024.0 * 1024.0);
                        return format!("{gb:.1} GB");
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(out) = run_command_output("sysctl", &["-n", "hw.memsize"]) {
            if let Ok(bytes) = out.trim().parse::<u64>() {
                let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                return format!("{gb:.1} GB");
            }
        }
    }

    "Unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_info_collect_and_format() {
        let info = SystemInfo::collect();
        assert!(!info.date.is_empty());
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
        assert!(!info.cpu.is_empty());
        assert!(!info.gpu.is_empty());
        assert!(!info.memory.is_empty());

        let prompt = info.to_system_prompt(Path::new("/test/workspace"));
        assert!(prompt.contains(&info.date));
        assert!(prompt.contains(&info.os));
        assert!(prompt.contains(&info.cpu));
        assert!(prompt.contains(&info.gpu));
        assert!(prompt.contains(&info.memory));
        assert!(prompt.contains("/test/workspace"));
    }
}
