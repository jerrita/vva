use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::home_dir;

/// 运行时偏好（与用户配置文件解耦）。路径：`~/.vva/state.json`。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppState {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_variant: Option<String>,
}

pub fn runtime_state_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".vva")
        .join("state.json")
}

impl AppState {
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            tracing::warn!("failed to create state dir {}: {error}", parent.display());
            return;
        }
        if let Err(error) = std::fs::write(path, bytes) {
            tracing::warn!("failed to write {}: {error}", path.display());
        }
    }

    pub fn persist_selection(path: &Path, model: &str, variant: Option<&str>) {
        if model.is_empty() {
            return;
        }
        Self {
            last_model: model.to_string(),
            last_variant: variant.map(str::to_string),
        }
        .save(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_selection_roundtrip() {
        let dir = std::env::temp_dir().join(format!("vva_state_{}", uuid::Uuid::new_v4().simple()));
        let path = dir.join("state.json");
        AppState::persist_selection(&path, "a/gpt-5.6-terra", Some("high"));
        let loaded = AppState::load(&path);
        assert_eq!(loaded.last_model, "a/gpt-5.6-terra");
        assert_eq!(loaded.last_variant.as_deref(), Some("high"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
