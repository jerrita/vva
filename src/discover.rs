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
