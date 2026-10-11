//! 入站附件落盘辅助：URL 规范化、文件名清洗、重名加后缀、流式下载。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;

use super::{ChannelError, FileSource, Result};

const MAX_FILENAME_CHARS: usize = 120;

/// 补全缺失的协议头（QQ 附件 URL 常省略 scheme）。
pub fn normalize_url(raw: &str) -> String {
    let url = raw.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{}", url.trim_start_matches('/'))
    }
}

/// 清洗文件名：去路径分量，非字母数字及 `._- ` 的字符换成 `_`，去首尾 `.`，限长 120 字符（保留扩展名）。
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim();
    if cleaned.is_empty() {
        return "file".into();
    }
    if cleaned.chars().count() <= MAX_FILENAME_CHARS {
        return cleaned.to_string();
    }
    let path = Path::new(cleaned);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.chars().count() < 16)
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(cleaned);
    let keep = MAX_FILENAME_CHARS - ext.chars().count();
    format!("{}{ext}", stem.chars().take(keep).collect::<String>())
}

/// 在 `dir` 下为 `filename` 选一个尚不存在的路径：重名时依次加 `_1`、`_2` 后缀。
pub async fn unique_dest(dir: &Path, filename: &str) -> PathBuf {
    let name = sanitize_filename(filename);
    let first = dir.join(&name);
    if !tokio::fs::try_exists(&first).await.unwrap_or(false) {
        return first;
    }
    let path = Path::new(&name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&name);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let mut n = 1u32;
    loop {
        let candidate = dir.join(format!("{stem}_{n}{ext}"));
        if !tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return candidate;
        }
        n += 1;
    }
}

/// 通过 HTTP GET 流式下载的附件来源。
#[derive(Debug)]
pub struct UrlSource {
    pub client: reqwest::Client,
    pub url: String,
}

#[async_trait]
impl FileSource for UrlSource {
    async fn download(&self, dest: &Path) -> Result<()> {
        let resp = self.client.get(&self.url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ChannelError::Api {
                status: status.as_u16(),
                path: self.url.clone(),
                message: "附件下载失败".into(),
            });
        }
        let mut file = tokio::fs::File::create(dest).await?;
        let mut stream = resp.bytes_stream();
        let written: Result<()> = async {
            while let Some(chunk) = stream.next().await {
                file.write_all(&chunk?).await?;
            }
            file.flush().await?;
            Ok(())
        }
        .await;
        if written.is_err() {
            drop(file);
            let _ = tokio::fs::remove_file(dest).await;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_is_sanitized() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("报告 v1.pdf"), "报告 v1.pdf");
        assert_eq!(sanitize_filename("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_filename("..."), "file");
        let long = format!("{}.png", "x".repeat(300));
        let out = sanitize_filename(&long);
        assert_eq!(out.chars().count(), 120);
        assert!(out.ends_with(".png"));
    }

    #[tokio::test]
    async fn duplicate_names_get_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let a = unique_dest(dir.path(), "a.txt").await;
        assert_eq!(a, dir.path().join("a.txt"));
        tokio::fs::write(&a, b"1").await.unwrap();
        let b = unique_dest(dir.path(), "a.txt").await;
        assert_eq!(b, dir.path().join("a_1.txt"));
        tokio::fs::write(&b, b"2").await.unwrap();
        assert_eq!(
            unique_dest(dir.path(), "a.txt").await,
            dir.path().join("a_2.txt")
        );
    }
}
