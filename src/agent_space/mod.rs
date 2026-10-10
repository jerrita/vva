//! Agent Space：常驻、经 IM 触达的个人 agent（设计见 docs/agent.md）。

pub mod binding;
pub mod channel;
pub mod command;
pub mod curator;
pub mod decisions;
pub mod lifecycle;
pub mod progress;
pub mod rollover;
pub mod router;
pub mod runtime;
pub mod schedule_tool;
pub mod scheduler;
pub mod send_file;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;

use self::channel::Channel;
use self::channel::qq::{QqChannel, QqOptions};
use self::decisions::DecisionsRouteModel;
use self::lifecycle::{Lifecycle, SWEEP_PERIOD};
use self::router::{RouteModel, Router, SmallRouteModel};
use self::runtime::Runtime;
use self::scheduler::ScheduleStore;
use crate::config::{AgentSpaceConfig, AppConfig, expand_tilde};
use crate::harness::{SessionManager, SessionPreamble, SessionToolFactory};
use crate::model::{SessionKind, SessionToggles};
use crate::storage::SessionStore;

/// agent space 固定工作区
const WORKSPACE: &str = "~/.vva/space";
/// 工作区下启动期创建的子目录：记忆库、curator 输入、项目级 skills / agents
const WORKSPACE_DIRS: [&str; 4] = ["memory", "curator-input", ".vva/skills", ".vva/agents"];

/// agent space 会话的功能开关：web_search 保持默认，team 关闭（不做 subagent 编排）
pub fn agent_toggles() -> SessionToggles {
    SessionToggles {
        team: false,
        ..SessionToggles::default()
    }
}

/// agent space 会话（排除 curator）首次落盘时注入记忆索引快照；无 `MEMORY.md` 不注入，
/// 读取失败告警后跳过（记忆缺席不应阻断会话）
fn memory_preamble(root: PathBuf) -> SessionPreamble {
    Arc::new(move |header| {
        if !header.agent_managed() || header.kind == SessionKind::Curator {
            return Vec::new();
        }
        curator::memory_reminder(&root)
            .unwrap_or_else(|error| {
                tracing::warn!("记忆索引读取失败，新会话不注入: {error}");
                None
            })
            .into_iter()
            .collect()
    })
}

/// 会话运行期工具：SendFile 与 Schedule 都只挂在带 IM 来源的顶层会话
fn tool_factory(channel: Arc<dyn Channel>, schedule: Arc<ScheduleStore>) -> SessionToolFactory {
    let send_file = send_file::session_tool_factory(channel.clone());
    let schedule = schedule_tool::session_tool_factory(channel.name().to_string(), schedule);
    Arc::new(move |header| {
        let mut tools = send_file(header);
        tools.extend(schedule(header));
        tools
    })
}

/// Decisions 后端的密钥：取自 `providers.<provider>.api_key`，缺失或为空即报错
fn decisions_api_key(config: &AppConfig, provider: &str) -> Result<String, String> {
    let p = config.providers.get(provider).ok_or_else(|| {
        format!("settings.agent.router.decisions.provider `{provider}` 未在 providers 中声明")
    })?;
    if p.api_key.is_empty() {
        return Err(format!(
            "providers.{provider}.api_key 为空（检查 ${{VAR}} 环境变量是否已设置）"
        ));
    }
    Ok(p.api_key.clone())
}

/// agent space 运行时：校验 `settings.agent` 后持有会话管理器
pub struct AgentSpace {
    pub config: AgentSpaceConfig,
    pub manager: Arc<SessionManager>,
    channel: Arc<dyn Channel>,
    schedule: Arc<ScheduleStore>,
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
        match &config.router.decisions {
            Some(d) => {
                decisions_api_key(&app_config, &d.provider)?;
            }
            None => {
                let small = app_config.settings.small_model.as_deref().ok_or(
                    "缺少 settings.small_model（router 所需；或配置 settings.agent.router.decisions）",
                )?;
                app_config
                    .resolve_model(small, None)
                    .map_err(|e| e.to_string())?;
            }
        }
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
        let qq = config
            .channels
            .qq
            .clone()
            .ok_or("settings.agent.channels 未配置 channel（目前仅支持 qq）")?;
        let channel: Arc<dyn Channel> = Arc::new(QqChannel::new(QqOptions {
            app_id: qq.app_id,
            secret: qq.secret,
            allowed_users: qq.allowed_users,
        }));
        let workspace = expand_tilde(WORKSPACE);
        std::fs::create_dir_all(&workspace)
            .map_err(|e| format!("创建 agent 工作区 {} 失败: {e}", workspace.display()))?;
        // 目录结构启动期建好：模型 Glob / Read 这些路径不应遇到 ENOENT
        for sub in WORKSPACE_DIRS {
            let dir = workspace.join(sub);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
        }
        // channel 先于 manager 构造，SendFile 经工厂按会话 origin 绑定聊天
        let root = workspace.clone();
        let schedule = ScheduleStore::open(&workspace)?;
        let manager = Arc::new(
            SessionManager::new(store, app_config, workspace, state_path)
                .with_tool_factory(tool_factory(channel.clone(), schedule.clone()))
                .with_preamble(memory_preamble(root)),
        );
        Ok(Self {
            config,
            manager,
            channel,
            schedule,
        })
    }

    /// 启动 daemon：channel 收消息、runtime 处理、scheduler 与 lifecycle 后台运行；
    /// ctrl-c 或 channel 退出时结束
    pub async fn run(&self) -> Result<(), String> {
        let channel = self.channel.clone();
        let dir = self.manager.workspace().to_path_buf();
        let app_config = self.manager.app_config();
        let model: Box<dyn RouteModel> = match &self.config.router.decisions {
            Some(d) => Box::new(DecisionsRouteModel::new(
                d.clone(),
                decisions_api_key(&app_config, &d.provider)?,
            )),
            None => Box::new(SmallRouteModel::new((*app_config).clone())),
        };
        let router = Router::new(model, dir.join("router.jsonl"));
        let runtime = Runtime::new(
            self.manager.clone(),
            self.config.clone(),
            channel.clone(),
            router,
            self.schedule.clone(),
        )?;
        let curator = runtime.curator();
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
        let scheduler = scheduler::spawn(self.schedule.clone(), event_tx);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Origin, SessionHeader};

    fn header(kind: SessionKind, origin: bool) -> SessionHeader {
        let mut header = crate::model::Session::new("/ws", None).header;
        header.kind = kind;
        header.origin = origin.then(|| Origin {
            channel: "qq".into(),
            chat_id: "c".into(),
        });
        header
    }

    #[test]
    fn preamble_injects_current_index_only_for_agent_sessions() {
        let dir = std::env::temp_dir().join(format!("vva-preamble-{}", uuid::Uuid::new_v4()));
        let preamble = memory_preamble(dir.clone());
        let topic = header(SessionKind::Topic, true);

        // 无 MEMORY.md：不注入任何内容
        assert!(preamble(&topic).is_empty());

        std::fs::create_dir_all(dir.join("memory")).unwrap();
        std::fs::write(dir.join("memory/MEMORY.md"), "- [偏好](pref.md)\n").unwrap();
        let texts = preamble(&topic);
        assert_eq!(texts.len(), 1);
        assert!(texts[0].starts_with("<system-reminder type=\"memory\">"));
        assert!(texts[0].contains("- [偏好](pref.md)"));
        assert!(texts[0].contains(&dir.join("memory").to_string_lossy().replace('\\', "/")));
        assert_eq!(preamble(&header(SessionKind::Oneshot, false)).len(), 1);

        // curator 与非 agent space 会话不注入
        assert!(preamble(&header(SessionKind::Curator, false)).is_empty());
        assert!(preamble(&header(SessionKind::Topic, false)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
