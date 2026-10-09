//! Agent Space：常驻、经 IM 触达的个人 agent（设计见 docs/agent.md）。

pub mod binding;
pub mod channel;
pub mod curator;
pub mod rollover;
pub mod router;
pub mod scheduler;

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{AgentSpaceConfig, AppConfig, expand_tilde};
use crate::harness::SessionManager;
use crate::model::SessionToggles;
use crate::storage::SessionStore;

/// agent space 固定工作区
const WORKSPACE: &str = "~/.vva/agent";

/// agent space 会话的功能开关：web_search 保持默认，team 关闭（不做 subagent 编排）
pub fn agent_toggles() -> SessionToggles {
    SessionToggles {
        team: false,
        ..SessionToggles::default()
    }
}

/// agent space 运行时：校验 `settings.agent` 后持有会话管理器
pub struct AgentSpace {
    pub config: AgentSpaceConfig,
    pub manager: Arc<SessionManager>,
}

impl AgentSpace {
    pub fn new(
        store: Arc<dyn SessionStore>,
        app_config: AppConfig,
        state_path: PathBuf,
    ) -> Result<Self, String> {
        let config = app_config
            .settings
            .agent
            .clone()
            .ok_or("缺少 settings.agent 配置，无法启动 agent space")?;
        config.validate()?;
        let workspace = expand_tilde(WORKSPACE);
        std::fs::create_dir_all(&workspace)
            .map_err(|e| format!("创建 agent 工作区 {} 失败: {e}", workspace.display()))?;
        let manager = Arc::new(SessionManager::new(
            store, app_config, workspace, state_path,
        ));
        Ok(Self { config, manager })
    }

    /// 骨架阶段：阻塞至 ctrl-c；daemon 逻辑由后续任务接入
    pub async fn run(&self) -> Result<(), String> {
        println!("agent space 骨架已就绪");
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| format!("监听 ctrl-c 失败: {e}"))
    }
}
