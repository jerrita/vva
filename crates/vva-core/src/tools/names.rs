//! 工具调用名：每个名字只对应一种协议，消费方按具体名字匹配。

pub const READ: &str = "Read";
pub const WRITE: &str = "Write";
pub const EDIT: &str = "Edit";
pub const GLOB: &str = "Glob";
pub const GREP: &str = "Grep";
pub const SHELL: &str = "Shell";
pub const APPLY_PATCH: &str = "apply_patch";
pub const HASHLINE_READ: &str = "hashline_read";
pub const HASHLINE_GREP: &str = "hashline_grep";
pub const HASHLINE_EDIT: &str = "hashline_edit";

/// 各编辑协议的只读文件工具全集（内置只读子代理白名单）
pub const FILE_READ_TOOLS: &[&str] = &[READ, GLOB, GREP, HASHLINE_READ, HASHLINE_GREP];

/// 各编辑协议的文件读写工具全集（内置读写子代理白名单）
pub const FILE_TOOLS: &[&str] = &[
    READ,
    WRITE,
    EDIT,
    GLOB,
    GREP,
    APPLY_PATCH,
    HASHLINE_READ,
    HASHLINE_GREP,
    HASHLINE_EDIT,
];
