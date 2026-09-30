//! `@` 文件引用：提示词 token 解析与工作区文件内容展开。
//!
//! 用户在提示词中书写 `@relative/path`（也支持绝对路径）引用工作区文件；
//! 提交时由 [`load_mention`] 读取文件并生成领域部件：
//! 文本文件展开为 [`ContentPart::FileRef`]，图片/PDF 转为原生 Image/File 附件
//! （与 Web 上传附件同构，经输入能力校验）。提示词原文保持不变。

use std::path::{Path, PathBuf};

use base64::Engine as _;
use tokio::io::AsyncReadExt;

use crate::model::{ContentPart, MediaSource};

/// 单个文本引用的内容上限（超出截断并标记 `truncated`）
pub const MAX_TEXT_BYTES: u64 = 128 * 1024;
/// 图片/PDF 引用的大小上限（媒体必须完整传输，不做截断）
pub const MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;

/// `@` 触发的前置字符：文本起点、空白或开括号（含全角）之后才视为引用，
/// 避免 `user@host` 邮箱或句中 `@` 误触发
fn is_mention_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '(' | '[' | '{' | '<' | '（' | '【' | '「' | '『' | '“' | '‘' | '《' | '〈'
        )
}

/// CJK 标点：不会出现在真实路径中，作为 token 的硬终止符（中文句式 `@a.md，然后` 常见）
fn is_cjk_punct(c: char) -> bool {
    matches!(
        c,
        '，' | '．'
            | '。'
            | '；'
            | '：'
            | '！'
            | '？'
            | '）'
            | '】'
            | '」'
            | '』'
            | '”'
            | '’'
            | '》'
            | '〉'
            | '…'
    )
}

/// token 末尾的 ASCII 收尾标点（自然语言中紧跟路径的标点，不属于路径本身）
fn is_trailing_punct(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '>' | '"' | '\''
    )
}

/// 光标处尚在编辑的引用：字符下标与 `@` 后至光标的查询串。
pub fn mention_at_cursor(text: &str, cursor: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    for at in (0..cursor).rev() {
        let c = chars[at];
        if c == '@' {
            return (at == 0 || is_mention_boundary(chars[at - 1]))
                .then(|| (at, chars[at + 1..cursor].iter().collect()));
        }
        if c.is_whitespace() || is_cjk_punct(c) {
            break;
        }
    }
    None
}

/// 提取提示词中全部 `@` 引用路径 token（按出现顺序去重）。
///
/// token 为 `@` 后连续的非空白字符，末尾剥离自然语言收尾标点；
/// 含空格的路径不支持（与 Claude Code 行为一致）。
pub fn extract_mention_paths(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut paths = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' || (i > 0 && !is_mention_boundary(chars[i - 1])) {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < chars.len() && !chars[end].is_whitespace() && !is_cjk_punct(chars[end]) {
            end += 1;
        }
        let mut token: String = chars[i + 1..end].iter().copied().collect();
        while token.chars().next_back().is_some_and(is_trailing_punct) {
            token.pop();
        }
        if !token.is_empty() && !token.starts_with('@') && !paths.contains(&token) {
            paths.push(token);
        }
        i = end.max(i + 1);
    }
    paths
}

/// 按 token 解析引用文件的实际路径：绝对路径原样，其余相对工作区根
fn resolve_path(workspace: &Path, token: &str) -> PathBuf {
    let candidate = Path::new(token);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace.join(candidate)
    }
}

/// 图片/PDF 扩展名 → 原生附件（与 Web 附件的 MIME 集合一致）
fn media_kind(path: &Path) -> Option<(&'static str, bool)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(("image/png", true)),
        "jpg" | "jpeg" => Some(("image/jpeg", true)),
        "gif" => Some(("image/gif", true)),
        "webp" => Some(("image/webp", true)),
        "pdf" => Some(("application/pdf", false)),
        _ => None,
    }
}

/// 引用未附加内容时的文本注记：告知模型用户引用了该文件，由其自行用工具处理
fn note_ref(token: &str, reason: &str) -> ContentPart {
    ContentPart::FileRef {
        path: token.to_string(),
        content: format!("[用户引用了此文件，内容未附加：{reason}；如需查看请使用工具读取]"),
        truncated: false,
    }
}

/// 模型输入能力不满足时的媒体引用降级注记（提交侧调用）
pub fn capability_note(token: &str) -> ContentPart {
    note_ref(token, "当前模型不支持该媒体输入")
}
/// 读取 `@token` 引用的文件并生成领域部件。仅路径不可读返回 Err（提交侧转 Notice）。
///
/// - 图片/PDF：完整读取转 Base64 原生附件（模型输入能力由提交侧校验并在不支持时降级）；
/// - 其余文件：按 UTF-8 文本读取，超出 [`MAX_TEXT_BYTES`] 截断并标记；
/// - 二进制文件、超出 [`MAX_MEDIA_BYTES`] 的媒体：不报错，降级为文本注记（模型自行处理）。
pub async fn load_mention(workspace: &Path, token: &str) -> Result<ContentPart, String> {
    let path = resolve_path(workspace, token);

    if let Some((mime, is_image)) = media_kind(&path) {
        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| format!("读取失败: {e}"))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_MEDIA_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| format!("读取失败: {e}"))?;
        if bytes.len() as u64 > MAX_MEDIA_BYTES {
            return Ok(note_ref(token, "媒体文件超过 20 MiB 上限"));
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(if is_image {
            ContentPart::Image {
                source: MediaSource::Base64 {
                    data,
                    mime_type: mime.into(),
                },
                mime_type: None,
            }
        } else {
            ContentPart::File {
                source: MediaSource::Base64 {
                    data,
                    mime_type: mime.into(),
                },
                mime_type: mime.into(),
                filename: path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| token.to_string()),
            }
        });
    }

    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("读取失败: {e}"))?;
    // 多读 1 字节用于判断截断
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("读取失败: {e}"))?;
    let truncated = bytes.len() as u64 > MAX_TEXT_BYTES;
    if truncated {
        bytes.truncate(MAX_TEXT_BYTES as usize);
    }
    if bytes.contains(&0u8) {
        return Ok(note_ref(token, "二进制文件"));
    }
    let content = String::from_utf8_lossy(&bytes).to_string();
    Ok(ContentPart::FileRef {
        path: token.to_string(),
        content,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_tokens_respect_boundaries_and_punctuation() {
        for text in [
            "@src/lib",
            "(@src/lib",
            "【@src/lib",
            "（@src/lib",
            "/check @src/lib",
        ] {
            let token = mention_at_cursor(text, text.chars().count()).unwrap();
            assert_eq!(token.1, "src/lib", "{text}");
            assert_eq!(extract_mention_paths(text), vec![token.1]);
        }
        for text in [
            "user@host",
            "@src/lib，然后",
            "【@src/lib】",
            "@src/lib ",
            "@@src/lib",
        ] {
            assert!(
                mention_at_cursor(text, text.chars().count()).is_none(),
                "{text}"
            );
        }
        assert_eq!(
            mention_at_cursor("看 @src/lib 后文", 10),
            Some((2, "src/lib".into()))
        );
        assert!(mention_at_cursor("@src/lib", 0).is_none());
    }

    #[tokio::test]
    async fn oversized_media_is_not_attached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.pdf");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_MEDIA_BYTES * 32)
            .unwrap();
        let part = load_mention(dir.path(), "large.pdf").await.unwrap();
        assert!(matches!(part, ContentPart::FileRef { content, .. } if content.contains("20 MiB")));
    }

    #[test]
    fn test_extract_basic_and_dedup() {
        let text = "看 @src/main.rs 与 @docs/a.md，再看 @src/main.rs";
        assert_eq!(
            extract_mention_paths(text),
            vec!["src/main.rs".to_string(), "docs/a.md".to_string()]
        );
    }

    #[test]
    fn test_extract_trailing_punctuation() {
        assert_eq!(
            extract_mention_paths("见 @a.md，然后 @b.rs。"),
            vec!["a.md", "b.rs"]
        );
        assert_eq!(extract_mention_paths("(@x.rs)"), vec!["x.rs"]);
    }

    #[test]
    fn test_extract_boundary_rules() {
        // 邮箱与非空白前缀不触发
        assert!(extract_mention_paths("联系 user@host.com").is_empty());
        assert!(extract_mention_paths("a@b").is_empty());
        // 行首/空白/换行/开括号后触发
        assert_eq!(extract_mention_paths("@a"), vec!["a"]);
        assert_eq!(extract_mention_paths("看 @a"), vec!["a"]);
        assert_eq!(extract_mention_paths("line1\n@a\nline3"), vec!["a"]);
        assert_eq!(extract_mention_paths("【@a】"), vec!["a"]);
        // 空 token 与 @@ 不触发
        assert!(extract_mention_paths("@ @@@").is_empty());
    }

    #[tokio::test]
    async fn test_load_text_and_missing() {
        let dir = std::env::temp_dir().join(format!("vva-mention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.txt"), "hello mention").unwrap();

        let part = load_mention(&dir, "src/a.txt").await.unwrap();
        match part {
            ContentPart::FileRef {
                path,
                content,
                truncated,
            } => {
                assert_eq!(path, "src/a.txt");
                assert_eq!(content, "hello mention");
                assert!(!truncated);
            }
            other => panic!("expected FileRef, got {other:?}"),
        }
        let part = load_mention(&dir, "missing.txt").await;
        assert!(part.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_load_media_and_binary() {
        let dir = std::env::temp_dir().join(format!("vva-mention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pic.png"), [0x89, b'P', b'N', b'G', 0u8, 1]).unwrap();
        std::fs::write(dir.join("blob.bin"), [1u8, 0u8, 2]).unwrap();
        std::fs::write(dir.join("doc.pdf"), b"%PDF-fake").unwrap();

        assert!(matches!(
            load_mention(&dir, "pic.png").await.unwrap(),
            ContentPart::Image { .. }
        ));
        assert!(matches!(
            load_mention(&dir, "doc.pdf").await.unwrap(),
            ContentPart::File { .. }
        ));
        match load_mention(&dir, "blob.bin").await.unwrap() {
            ContentPart::FileRef { path, content, .. } => {
                assert_eq!(path, "blob.bin");
                assert!(content.contains("二进制文件"));
            }
            other => panic!("expected FileRef note, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_load_truncation() {
        let dir = std::env::temp_dir().join(format!("vva-mention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = "x".repeat(MAX_TEXT_BYTES as usize + 100);
        std::fs::write(dir.join("big.txt"), &big).unwrap();
        match load_mention(&dir, "big.txt").await.unwrap() {
            ContentPart::FileRef {
                content, truncated, ..
            } => {
                assert!(truncated);
                assert_eq!(content.len(), MAX_TEXT_BYTES as usize);
            }
            other => panic!("expected FileRef, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
