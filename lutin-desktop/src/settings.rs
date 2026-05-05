//! Desktop-local settings.
//!
//! Stores per-machine chrome state the user controls from the Settings
//! view: which control-panel(s) we know about and which one to dial on
//! startup. Distinct from the global `<global>/settings.toml` — that
//! tier is owned by the control panel and shared across every desktop
//! that connects to it. This file is for the chrome only.
//!
//! Persisted as JSON at `<config_dir>/lutin/desktop.json`
//! (`~/.config/lutin/desktop.json` on Linux). Missing file → defaults.
//! Parse errors → defaults; we don't want a corrupt settings file to
//! brick startup, and the user can re-save from the UI to overwrite it.
//!
//! Env vars (`LUTIN_CP_ADDR`, `LUTIN_CP_TOKEN`) still take precedence
//! over settings so an operator can override without editing the file.
//! Keybinds are intentionally out of scope here — add later when there
//! are enough chrome shortcuts to warrant configuration.
//!
//! Modeled after `../lutin/desktop/src/settings/mod.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One named control-panel endpoint.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub name: String,
    /// `host:port` — same shape as `LUTIN_CP_ADDR`. We dial `ws://` over it.
    pub addr: String,
    /// ControlPanel-scoped token minted via `lutin-cp-mint`.
    pub token: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopSettings {
    /// Name of the connection to use on startup. Falls back to the
    /// first entry when the named one is missing.
    pub default: String,
    pub connections: Vec<ConnectionProfile>,
}

impl DesktopSettings {
    /// Resolve the active connection: the one whose `name` matches
    /// `default`, or the first if the default is missing/blank.
    pub fn active(&self) -> Option<&ConnectionProfile> {
        self.connections
            .iter()
            .find(|c| c.name == self.default)
            .or_else(|| self.connections.first())
    }

    pub fn load() -> Self {
        let path = settings_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lutin")
        .join("desktop.json")
}
