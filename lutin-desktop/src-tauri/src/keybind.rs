//! Global hotkey registry.
//!
//! Owns OS-level shortcut registration via `tauri-plugin-global-shortcut`
//! and dispatches presses/releases to a single in-process handler. The
//! handler hands the event off to `dispatch::dispatch`, which routes
//! into the audio / transcription / clipboard pipeline. Wake-word will
//! reuse the same dispatch entry as a different `Trigger` variant.
//!
//! `KeyBind.combo` is parsed into a real `Shortcut` at the boundary
//! (`parse` for settings_set, `parse_lossy` for startup) so the registry
//! never holds unparseable combo strings. Lookups compare `Shortcut`
//! values directly — no string round-trip on the keyboard hot path.

use std::str::FromStr;
use std::sync::Mutex;

use serde::Serialize;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tracing::{debug, warn};

use crate::dispatch::{self, Trigger};
use crate::settings::KeyBind;

/// In-process registry of currently-registered combos. Stored as a
/// `Vec` because the working set is ~5–20 entries and linear scan
/// beats hashing at that scale (no hashing per lookup, contiguous
/// memory, deterministic order). Held inside `AppState` behind a
/// Mutex; the lock is taken only for the duration of one comparison
/// or one swap, never across `.await` or OS calls.
#[derive(Default)]
pub struct KeybindRegistry {
    binds: Mutex<Vec<(Shortcut, KeyBind)>>,
}

impl KeybindRegistry {
    fn lookup(&self, key: &Shortcut) -> Option<KeyBind> {
        self.binds
            .lock()
            .expect("keybind registry poisoned")
            .iter()
            .find(|(s, _)| s == key)
            .map(|(_, b)| b.clone())
    }
}

/// Phase of a key event. Mirrors the plugin's `ShortcutState` 1:1 but
/// is our own type so the JS-facing payload is stable if the plugin
/// crate renames its enum.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShortcutPhase {
    Down,
    Up,
}

impl From<ShortcutState> for ShortcutPhase {
    fn from(s: ShortcutState) -> Self {
        match s {
            ShortcutState::Pressed => ShortcutPhase::Down,
            ShortcutState::Released => ShortcutPhase::Up,
        }
    }
}

/// Build the plugin with a handler that looks the combo up in the
/// active backend's registry and hands the (Action, Target, Phase)
/// tuple to dispatch. Runs on the global-shortcut thread; dispatch
/// immediately offloads to tokio so we never block here.
///
/// The handler tolerates the backend OnceLock not being set yet
/// (events arriving during the brief window between Tauri starting
/// and `setup()` finishing) by silently dropping — these are spurious
/// echoes, not bound combos.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(move |app, shortcut, event| {
            let state = app.state::<crate::AppState>();
            let Some(crate::KeybindBackend::Plugin(reg)) = state.keybind_backend.get() else {
                return;
            };
            let Some(bind) = reg.lookup(shortcut) else {
                debug!(combo = %shortcut.into_string(), "keybind fired with no registry entry");
                return;
            };
            dispatch::dispatch(
                app.clone(),
                Trigger::Hotkey,
                bind.action,
                bind.target,
                event.state().into(),
            );
        })
        .build()
}

/// Returned from `parse` when a user-supplied combo string can't be
/// parsed as a `Shortcut`. Caller (settings_set) surfaces this back
/// to the JS settings UI so the user knows which entry to fix.
pub struct ParseError {
    pub combo: String,
    pub source: String,
}

/// Strict parse: all combos must be valid. Run at the settings_set
/// boundary so an invalid combo is rejected before we save the file
/// or touch OS registration. Returns the parsed list paired with the
/// original `KeyBind` so reconcile can keep action/target metadata.
pub fn parse(binds: &[KeyBind]) -> Result<Vec<(Shortcut, KeyBind)>, ParseError> {
    binds
        .iter()
        .map(|b| {
            Shortcut::from_str(&b.combo)
                .map(|s| (s, b.clone()))
                .map_err(|e| ParseError {
                    combo: b.combo.clone(),
                    source: e.to_string(),
                })
        })
        .collect()
}

/// Lossy parse: skip + log unparseable entries. Used at startup, where
/// we can't return errors to a user-facing call site — a single bad
/// combo in the persisted file shouldn't disable every other hotkey.
pub fn parse_lossy(binds: &[KeyBind]) -> Vec<(Shortcut, KeyBind)> {
    binds
        .iter()
        .filter_map(|b| match Shortcut::from_str(&b.combo) {
            Ok(s) => Some((s, b.clone())),
            Err(e) => {
                warn!(combo = %b.combo, error = %e, "skip unparseable keybind");
                None
            }
        })
        .collect()
}

/// Reconcile registered shortcuts with `desired`. Idempotent: safe on
/// every settings_set or at startup. Combos that fail to register at
/// the OS layer are returned as canonicalized strings so the caller
/// can surface them. Combos in `desired` that aren't currently
/// registered get registered; combos currently registered that aren't
/// in `desired` get unregistered. Already-registered combos stay
/// registered without re-registration.
pub fn reconcile<R: Runtime>(
    app: &AppHandle<R>,
    registry: &KeybindRegistry,
    desired: Vec<(Shortcut, KeyBind)>,
) -> Vec<String> {
    let shortcuts = app.global_shortcut();

    // Snapshot the existing key set under a brief lock, so OS calls
    // (which can take milliseconds on Wayland) don't stretch the
    // critical section.
    let existing: Vec<Shortcut> = registry
        .binds
        .lock()
        .expect("keybind registry poisoned")
        .iter()
        .map(|(s, _)| s.clone())
        .collect();

    for s in &existing {
        if !desired.iter().any(|(d, _)| d == s) {
            if let Err(e) = shortcuts.unregister(s.clone()) {
                warn!(combo = %s.into_string(), error = %e, "unregister failed");
            }
        }
    }

    let mut failed: Vec<String> = Vec::new();
    let mut applied: Vec<(Shortcut, KeyBind)> = Vec::with_capacity(desired.len());
    for (s, bind) in desired {
        let already = existing.iter().any(|e| e == &s);
        if !already {
            if let Err(e) = shortcuts.register(s.clone()) {
                warn!(combo = %s.into_string(), error = %e, "register failed");
                failed.push(s.into_string());
                continue;
            }
        }
        applied.push((s, bind));
    }

    *registry.binds.lock().expect("keybind registry poisoned") = applied;
    failed
}
