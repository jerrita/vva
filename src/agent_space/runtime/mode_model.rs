//! agent 模式模型：新会话使用的模型规格与推理档位，持久化于 `<workspace>/model.json`，优先于配置文件。

use serde::{Deserialize, Serialize};

use super::Runtime;
use crate::config::AgentSpaceConfig;
use crate::harness::SessionManager;

pub(super) const MODE_MODEL_FILE: &str = "model.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ModeModel {
    pub(super) model: String,
    #[serde(default)]
    pub(super) variant: Option<String>,
}

/// 校验模型：可解析且声明 `limit.context`（rollover 需要）；返回解析后的完整规格
pub(super) fn check_mode_model(
    manager: &SessionManager,
    model: &str,
    variant: Option<String>,
) -> Result<ModeModel, String> {
    let config = manager.app_config();
    let resolved = config
        .resolve_model(model, variant.as_deref())
        .map_err(|e| e.to_string())?;
    if resolved.model_config.limit.is_none() {
        return Err(format!(
            "模型 {} 未声明 limit.context，rollover 无法判定",
            resolved.full_spec
        ));
    }
    Ok(ModeModel {
        model: resolved.full_spec,
        variant,
    })
}

/// 启动期：`model.json` 优先，否则取配置（`settings.agent.model` → `settings.default`）
pub(super) fn load_mode_model(
    config: &AgentSpaceConfig,
    manager: &SessionManager,
) -> Result<ModeModel, String> {
    let path = manager.workspace().join(MODE_MODEL_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let saved: ModeModel = serde_json::from_str(&raw)
                .map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;
            check_mode_model(manager, &saved.model, saved.variant).map_err(|e| {
                format!(
                    "{}: {e}（删除该文件可恢复配置文件中的模型）",
                    path.display()
                )
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let model = config
                .model
                .clone()
                .unwrap_or_else(|| manager.app_config().settings.default.clone());
            Ok(ModeModel {
                model,
                variant: None,
            })
        }
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

impl Runtime {
    pub(super) fn mode_model(&self) -> ModeModel {
        self.mode_model.lock().expect("mode_model").clone()
    }

    /// 落盘并设置已经 [`check_mode_model`] 校验的 agent 模式模型
    pub(super) fn save_mode_model(&self, next: ModeModel) -> Result<(), String> {
        let raw = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        let path = self.manager.workspace().join(MODE_MODEL_FILE);
        std::fs::write(&path, raw).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
        *self.mode_model.lock().expect("mode_model") = next;
        Ok(())
    }
}
