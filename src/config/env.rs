//! env 文件：dotenv 子集解析。全局 `~/.vva/env` 启动时写入进程环境，
//! 项目 `<workspace>/.vva/env` 每次执行 shell 命令时读取并注入子进程。

use std::path::Path;

use super::parser::interpolate_with;

/// 工作区内的项目 env 文件相对路径
pub const PROJECT_ENV_FILE: &str = ".vva/env";

/// 读取并解析 env 文件；文件不存在时返回空表
pub fn load_env_file(path: &Path) -> Result<Vec<(String, String)>, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => parse_env(&content).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// 读取工作区项目 env（`<workspace>/.vva/env`）
pub fn workspace_env(workspace: &Path) -> Result<Vec<(String, String)>, String> {
    load_env_file(&workspace.join(PROJECT_ENV_FILE))
}

/// 解析 dotenv 子集：`KEY=VALUE`、`#` 注释、可选 `export` 前缀；
/// 双引号值支持 `\n` `\t` `\"` `\\` 转义，单引号值原样保留；
/// 未加引号与双引号值展开 `${VAR}`（先查本文件已定义的键，再查进程环境）
pub fn parse_env(content: &str) -> Result<Vec<(String, String)>, String> {
    let mut vars: Vec<(String, String)> = Vec::new();
    for (index, raw) in content.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {line_no}: expected KEY=VALUE"))?;
        let key = key.trim();
        if !is_valid_key(key) {
            return Err(format!("line {line_no}: invalid key '{key}'"));
        }
        let lookup = |name: &str| {
            vars.iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var(name).ok())
        };
        let value =
            parse_value(value.trim_start(), lookup).map_err(|e| format!("line {line_no}: {e}"))?;
        vars.push((key.to_string(), value));
    }
    Ok(vars)
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_value(value: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    if let Some(rest) = value.strip_prefix('\'') {
        let end = rest.find('\'').ok_or("unterminated single quote")?;
        ensure_trailing(&rest[end + 1..])?;
        return Ok(rest[..end].to_string());
    }
    if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    ensure_trailing(&rest[i + 1..])?;
                    return Ok(interpolate_with(&out, lookup));
                }
                '\\' => match chars.next().map(|(_, c)| c) {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => return Err(format!("unsupported escape '\\{other}'")),
                    None => break,
                },
                _ => out.push(c),
            }
        }
        return Err("unterminated double quote".into());
    }
    // 未加引号：空白后的 `#` 起为行内注释
    let end = value
        .char_indices()
        .find(|&(i, c)| c == '#' && value[..i].ends_with(char::is_whitespace))
        .map_or(value.len(), |(i, _)| i);
    Ok(interpolate_with(value[..end].trim_end(), lookup))
}

/// 引号闭合后仅允许空白或行内注释
fn ensure_trailing(rest: &str) -> Result<(), String> {
    let rest = rest.trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!("unexpected content after closing quote: '{rest}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quotes_comments_and_interpolation() {
        let vars = parse_env(
            "# comment\n\
             export A=1\n\
             B = two words # trailing\n\
             C=\"x\\ny ${A}\" # c\n\
             D='raw ${A}'\n\
             E=${A}-${B}\n",
        )
        .unwrap();
        let get = |k: &str| vars.iter().find(|(key, _)| key == k).unwrap().1.as_str();
        assert_eq!(get("A"), "1");
        assert_eq!(get("B"), "two words");
        assert_eq!(get("C"), "x\ny 1");
        assert_eq!(get("D"), "raw ${A}");
        assert_eq!(get("E"), "1-two words");
    }

    #[test]
    fn rejects_malformed_lines_with_line_number() {
        assert!(parse_env("A=1\nnot a pair").unwrap_err().contains("line 2"));
        assert!(parse_env("1A=x").unwrap_err().contains("invalid key"));
        assert!(parse_env("A=\"open").unwrap_err().contains("unterminated"));
        assert!(
            parse_env("A='x' y")
                .unwrap_err()
                .contains("after closing quote")
        );
    }
}
