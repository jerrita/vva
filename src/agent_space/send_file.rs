//! SendFile 工具：经当前会话所在渠道把本地文件发给用户。
//!
//! 渠道绑定、运行期按会话注入（见 [`session_tool_factory`]），不属于静态注册表；
//! subagent 会话不挂载。渠道返回的错误（如 QQ guild / dm 不支持上传）原样作为工具错误，由模型转述。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::channel::{Channel, Outbound, OutboundFile};
use crate::config::expand_tilde;
use crate::harness::SessionToolFactory;
use crate::model::SessionHeader;
use crate::tools::filesystem::resolve_path;
use crate::tools::{Tool, parse_args};

struct SendFileTool {
    channel: Arc<dyn Channel>,
    /// 目标聊天，形如 `channel:chat_id`
    binding: String,
    workspace: PathBuf,
}

#[async_trait]
impl Tool for SendFileTool {
    fn name(&self) -> &str {
        "SendFile"
    }

    fn description(&self) -> &str {
        "Send a local file to the user through the current chat channel. Accepts workspace-relative, absolute, and ~ paths. Use this when the user should receive the file itself instead of its content."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path of the file to send"},
                "caption": {"type": "string", "description": "Optional short text accompanying the file"}
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Arguments {
            path: String,
            caption: Option<String>,
        }
        let args: Arguments = parse_args(arguments, "SendFile")?;
        let expanded = expand_tilde(&args.path);
        let path = resolve_path(&self.workspace, &expanded.to_string_lossy());
        let metadata = std::fs::metadata(&path)
            .map_err(|e| format!("Failed to access {}: {e}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("Not a regular file: {}", path.display()));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.channel
            .send(Outbound::File(OutboundFile {
                binding: self.binding.clone(),
                path,
                caption: args.caption.unwrap_or_default(),
                reply_to: None,
            }))
            .await
            .map_err(|e| e.to_string())?;
        Ok(format!("已发送 {name} ({} bytes)", metadata.len()))
    }
}

/// 会话运行期工具的挂载条件：带 IM 来源的顶层会话，返回其聊天 `channel:chat_id`
pub(super) fn chat_binding(header: &SessionHeader) -> Option<String> {
    let origin = header.origin.as_ref().filter(|_| header.parent.is_none())?;
    Some(format!("{}:{}", origin.channel, origin.chat_id))
}

/// 会话工具工厂：为带 IM 来源的顶层会话挂载绑定其聊天的 SendFile
pub fn session_tool_factory(channel: Arc<dyn Channel>) -> SessionToolFactory {
    Arc::new(move |header: &SessionHeader| {
        let Some(binding) = chat_binding(header) else {
            return Vec::new();
        };
        let tool: Arc<dyn Tool> = Arc::new(SendFileTool {
            channel: channel.clone(),
            binding,
            workspace: header.workspace_root.clone(),
        });
        vec![tool]
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::sync::mpsc;

    use super::super::channel::Inbound;
    use super::*;
    use crate::model::{Origin, Session, SessionKind};

    #[derive(Default)]
    struct FakeChannel {
        sent: Mutex<Vec<OutboundFile>>,
    }

    #[async_trait]
    impl Channel for FakeChannel {
        fn name(&self) -> &'static str {
            "qq"
        }
        async fn send(&self, msg: Outbound) -> super::super::channel::Result<Vec<String>> {
            let Outbound::File(file) = msg else {
                panic!("expected file");
            };
            self.sent.lock().unwrap().push(file);
            Ok(vec!["m1".into()])
        }
        async fn run(
            self: Arc<Self>,
            _: mpsc::Sender<Inbound>,
        ) -> super::super::channel::Result<()> {
            Ok(())
        }
    }

    fn fixture() -> (Arc<FakeChannel>, SendFileTool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let channel = Arc::new(FakeChannel::default());
        let tool = SendFileTool {
            channel: channel.clone(),
            binding: "qq:c2c:u1".into(),
            workspace: dir.path().to_path_buf(),
        };
        (channel, tool, dir)
    }

    #[tokio::test]
    async fn sends_relative_path_with_caption() {
        let (channel, tool, dir) = fixture();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let out = tool
            .execute(r#"{"path":"a.txt","caption":"看看"}"#)
            .await
            .unwrap();
        assert_eq!(out, "已发送 a.txt (5 bytes)");
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent[0].binding, "qq:c2c:u1");
        assert_eq!(sent[0].path, dir.path().join("a.txt"));
        assert_eq!(sent[0].caption, "看看");
    }

    #[tokio::test]
    async fn missing_file_and_directory_are_errors() {
        let (channel, tool, dir) = fixture();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let err = tool.execute(r#"{"path":"nope.txt"}"#).await.unwrap_err();
        assert!(err.contains("nope.txt"));
        let err = tool.execute(r#"{"path":"sub"}"#).await.unwrap_err();
        assert!(err.contains("Not a regular file"));
        assert!(channel.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn chat_binding_only_for_top_level_origin_sessions() {
        let header = |kind, origin: bool, continued: bool| {
            let mut header = Session::new("/w", None).header;
            header.kind = kind;
            header.origin = origin.then(|| Origin {
                channel: "qq".into(),
                chat_id: "c2c:u1".into(),
            });
            header.continued_from = continued.then(|| "sess_00000".into());
            header
        };
        let topic = header(SessionKind::Topic, true, false);
        assert_eq!(chat_binding(&topic).as_deref(), Some("qq:c2c:u1"));
        assert!(chat_binding(&header(SessionKind::Oneshot, true, false)).is_some());
        // rollover 接替会话不是 subagent，仍挂载
        assert!(chat_binding(&header(SessionKind::Topic, true, true)).is_some());
        // curator 无 IM 来源
        assert!(chat_binding(&header(SessionKind::Curator, false, false)).is_none());
        let mut sub = topic;
        sub.parent = Some("sess_00000".into());
        assert!(chat_binding(&sub).is_none());
    }
}
