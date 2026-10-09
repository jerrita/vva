//! Agent Space：常驻、经 IM 触达的个人 agent（设计见 docs/agent.md）。

pub mod binding;
pub mod channel;
pub mod command;
pub mod curator;
pub mod lifecycle;
pub mod rollover;
pub mod router;
pub mod runtime;
pub mod scheduler;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;

use self::channel::Channel;
use self::channel::qq::{QqChannel, QqOptions};
use self::curator::Curator;
use self::lifecycle::{Lifecycle, SWEEP_PERIOD};
use self::router::{Router, SmallRouteModel};
use self::runtime::Runtime;
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
        // router 与主模型在启动期校验，避免首条消息才暴露配置错误
        let small = app_config
            .settings
            .small_model
            .as_deref()
            .ok_or("缺少 settings.small_model（router 所需）")?;
        app_config
            .resolve_model(small, None)
            .map_err(|e| e.to_string())?;
        let main = config
            .model
            .as_deref()
            .unwrap_or(&app_config.settings.default);
        let main = app_config
            .resolve_model(main, None)
            .map_err(|e| e.to_string())?;
        if main.model_config.limit.is_none() {
            return Err(format!(
                "模型 {} 未声明 limit.context，rollover 无法判定",
                main.full_spec
            ));
        }
        let workspace = expand_tilde(WORKSPACE);
        std::fs::create_dir_all(&workspace)
            .map_err(|e| format!("创建 agent 工作区 {} 失败: {e}", workspace.display()))?;
        let manager = Arc::new(SessionManager::new(
            store, app_config, workspace, state_path,
        ));
        Ok(Self { config, manager })
    }

    /// 启动 daemon：channel 收消息、runtime 处理、scheduler 与 lifecycle 后台运行；
    /// ctrl-c 或 channel 退出时结束
    pub async fn run(&self) -> Result<(), String> {
        let qq = self
            .config
            .channels
            .qq
            .clone()
            .ok_or("settings.agent.channels 未配置 channel（目前仅支持 qq）")?;
        let channel: Arc<dyn Channel> = Arc::new(QqChannel::new(QqOptions {
            app_id: qq.app_id,
            secret: qq.secret,
            allowed_users: qq.allowed_users,
        }));
        let dir = self.manager.workspace().to_path_buf();
        let router = Router::new(
            Box::new(SmallRouteModel::new((*self.manager.app_config()).clone())),
            dir.join("router.jsonl"),
        );
        let runtime = Runtime::new(
            self.manager.clone(),
            self.config.clone(),
            channel.clone(),
            router,
        )?;
        let curator = Arc::new(Curator::for_manager(&self.manager, runtime.model())?);
        let notify = {
            let runtime = runtime.clone();
            Arc::new(move |report| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.push_curator(report).await });
            })
        };
        let lifecycle = Arc::new(Lifecycle::new(
            self.manager.clone(),
            curator,
            self.config.clone(),
            notify,
        ))
        .spawn(SWEEP_PERIOD);

        let (inbound_tx, inbound_rx) = mpsc::channel(64);
        let (event_tx, event_rx) = mpsc::channel(16);
        let scheduler = scheduler::spawn(&dir, event_tx);
        let mut channel_task = tokio::spawn(channel.run(inbound_tx));
        let runtime_task = tokio::spawn(runtime.run(inbound_rx, event_rx));
        println!("agent space 已启动 ({})，ctrl-c 退出", dir.display());

        let outcome = tokio::select! {
            signal = tokio::signal::ctrl_c() => signal.map_err(|e| format!("监听 ctrl-c 失败: {e}")),
            ended = &mut channel_task => Err(match ended {
                Ok(Ok(())) => "channel 意外退出".to_string(),
                Ok(Err(e)) => format!("channel 运行失败: {e}"),
                Err(e) => format!("channel 任务异常: {e}"),
            }),
        };
        for task in [&lifecycle, &scheduler, &runtime_task] {
            task.abort();
        }
        channel_task.abort();
        outcome
    }
}
