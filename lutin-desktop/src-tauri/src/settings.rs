//! Desktop-local settings.
//!
//! Stores per-machine chrome state the user controls from the Settings
//! view: which control-panel(s) we know about and which one to dial on
//! startup. Persisted as JSON at `<config_dir>/lutin/desktop.json`
//! (`~/.config/lutin/desktop.json` on Linux). Missing or malformed file
//! → defaults; we don't want a corrupt settings file to brick startup.

use std::path::PathBuf;

use lutin_control_protocol::{SttConfig, WorkflowId};
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

/// What audio pipeline a hotkey drives. The `Target` (sibling field on
/// `KeyBind`) decides where the resulting text goes — these two axes
/// are intentionally orthogonal so adding wake-word later is just
/// another trigger source feeding the same routing table.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Hold-to-talk. Down → start capture. Up → stop + transcribe.
    Ptt,
    // Reserved (additive — adding doesn't break existing settings):
    //   Dictate          — tap to start, second tap or silence to stop
    //   OpenMicToggle    — flip always-on listening
    //   ChromeAction { name: String } — non-audio chrome operations
}

/// Where the transcribed text lands. Resolution of `ActiveWorkflow`
/// happens at fire-time using the React store's reported active
/// session id; null falls through to clipboard so audio isn't lost.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    ActiveWorkflow,
    Workflow { workflow: WorkflowId },
    Clipboard,
    // Reserved: TypeIntoFocused (needs `enigo`).
}

/// One user-bound shortcut. `combo` is in the accelerator format
/// understood by `tauri-plugin-global-shortcut` (e.g. `"Numpad1"`,
/// `"CommandOrControl+Shift+M"`). The on-disk shape — chrome's
/// in-memory map is derived from this list on load + on settings_set.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyBind {
    pub combo: String,
    pub action: Action,
    pub target: Target,
}

/// Per-machine audio device pinning. Names are cpal device names
/// (`Device::name()`); `None` means "use the host default", so settings
/// stay portable across machines where the named device doesn't exist.
/// Devices are matched by exact name on apply; if the saved name isn't
/// present (USB mic unplugged, etc.) we fall back to the default rather
/// than failing — losing PTT/TTS because of a transient device change
/// is worse than ignoring the saved preference.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub input: Option<String>,
    pub output: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopSettings {
    /// Name of the connection to use on startup. Falls back to the
    /// first entry when the named one is missing.
    pub default: String,
    pub connections: Vec<ConnectionProfile>,
    /// User-configured global hotkeys. Empty list means "no hotkeys";
    /// callers wanting a sensible default for first-run UX can call
    /// `effective_keybinds()` instead which seeds a PTT default.
    pub keybinds: Vec<KeyBind>,
    /// STT backend selection + per-backend prefs. Defaults to whisper
    /// large-v3-turbo with autodetect; first PTT after a fresh install
    /// triggers the model download. Switching to Parakeet is just a
    /// `{ "kind": "parakeet", "model": "tdt-0.6b-v3" }` swap here —
    /// CP downloads the ONNX bundle on first use.
    pub stt: SttConfig,
    /// Pinned input/output devices. Defaults to host defaults.
    pub audio: AudioSettings,
}

impl DesktopSettings {
    /// Bindings to actually register with the OS. Empty list seeds the
    /// built-in PTT defaults (`Numpad1 → ActiveWorkflow`,
    /// `Numpad2 → Clipboard`) so first-run users get both in-app
    /// dictation and a global "transcribe to clipboard" hotkey without
    /// touching settings. Trade-off: an empty list cannot mean "no
    /// hotkeys" — to opt out the user binds a single throwaway combo.
    /// v2 may switch to `Option<Vec<KeyBind>>` if that turns out to
    /// matter.
    pub fn effective_keybinds(&self) -> Vec<KeyBind> {
        if self.keybinds.is_empty() {
            vec![
                KeyBind {
                    combo: "Numpad1".to_owned(),
                    action: Action::Ptt,
                    target: Target::ActiveWorkflow,
                },
                KeyBind {
                    combo: "Numpad2".to_owned(),
                    action: Action::Ptt,
                    target: Target::Clipboard,
                },
            ]
        } else {
            self.keybinds.clone()
        }
    }

    pub fn active(&self) -> Option<&ConnectionProfile> {
        self.connections
            .iter()
            .find(|c| c.name == self.default)
            .or_else(|| self.connections.first())
    }

    pub fn load() -> Self {
        let path = settings_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "settings read failed");
                return Self::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                // Don't silently overwrite a malformed file — preserve
                // it next to the original so the user (or future-you)
                // can recover the keybinds / whisper config / provider
                // tokens that just got rejected.
                let backup = path.with_extension("json.bad");
                let _ = std::fs::rename(&path, &backup);
                tracing::error!(
                    error = %e,
                    path = %path.display(),
                    backup = %backup.display(),
                    "settings parse failed; preserved as .bad and falling back to defaults"
                );
                Self::default()
            }
        }
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
