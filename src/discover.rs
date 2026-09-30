//! agents 与 skills 共用的发现基建：目录优先级与 name 校验。
//!
//! 两者的目录布局同构（全局 `~/.agents` < `~/.vva`，项目 `.agents` < `.vva`，
//! 同名时后扫描的目录覆盖先扫描的），只差最后一段子目录名；name 约束也相同
//! （1-64 字符，小写字母/数字与非连续连字符，且必须与文件主干或父目录名一致）。
//! 收集语义不同（agents 在高优先级定义失效时移除同名条目，skills 只告警），
//! 留在各自模块。

use std::path::{Path, PathBuf};

/// 全局目录（优先级升序）：`~/.agents/<leaf>`、`~/.vva/<leaf>`；无主目录时为空
pub fn global_dirs(leaf: &str) -> Vec<PathBuf> {
    crate::config::home_dir()
        .map(|home| {
            vec![
                home.join(".agents").join(leaf),
                home.join(".vva").join(leaf),
            ]
        })
        .unwrap_or_default()
}

/// 项目目录（优先级升序）：`<workspace>/.agents/<leaf>`、`<workspace>/.vva/<leaf>`
pub fn project_dirs(workspace: &Path, leaf: &str) -> [PathBuf; 2] {
    [
        workspace.join(".agents").join(leaf),
        workspace.join(".vva").join(leaf),
    ]
}

/// name 约束：1-64 字符，仅小写字母/数字与非连续连字符，且必须与 `expected` 一致。
/// `kind` 用于错误文案（如 "agent" / "skill"），`expected_label` 描述期望来源
/// （如 "file name" / "directory name"）。
pub fn validate_name(
    name: &str,
    expected: &str,
    kind: &str,
    expected_label: &str,
) -> Result<(), String> {
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
            "Invalid {kind} name '{name}': must be 1-64 lowercase letters, digits or single hyphens"
        ));
    }
    if name != expected {
        return Err(format!(
            "{kind_cap} name '{name}' must match its {expected_label} '{expected}'",
            kind_cap = capitalize(kind)
        ));
    }
    Ok(())
}

/// description 约束：非空且不超过 1024 字符
pub fn validate_description(description: &str, kind: &str, name: &str) -> Result<(), String> {
    if description.trim().is_empty() {
        return Err(format!(
            "{} '{name}' has an empty description",
            capitalize(kind)
        ));
    }
    if description.chars().count() > 1024 {
        return Err(format!(
            "{} '{name}' description exceeds 1024 characters",
            capitalize(kind)
        ));
    }
    Ok(())
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}
