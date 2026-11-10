//! skill / agent 文件的通用字段校验。

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
