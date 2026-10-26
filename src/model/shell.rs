use serde::{Deserialize, Serialize};

/// 有界、已处理控制序列的 Shell 展示输出。start 为已淘汰的 UTF-8 字节数。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellOutput {
    pub revision: u64,
    pub start: u64,
    pub text: String,
    pub total_bytes: u64,
    pub finished: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
