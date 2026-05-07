//! Floating "listening / transcribing" indicator window.
//!
//! Owns the lifecycle of the second Tauri webview defined in
//! `tauri.conf.json` (label `"overlay"`): unmaps it on app start
//! (configured `"visible": false`), shows it on PTT down, swaps the
//! phase as transcription runs, hides it after a brief grace period
//! on terminal phases. The webview itself just listens for
//! `overlay:phase` events and renders a pill — this module is the
//! Rust-side controller.
//!
//! All show/hide work is best-effort. If the window can't be
//! resolved (very early during boot, or if the user closed it
//! manually) we log and continue — the indicator missing one frame
//! is much better than a panic on the keyboard hot path.
//!
//! Auto-hide uses a per-call generation counter (`HIDE_GENERATION`):
//! every show bumps it, the delayed-hide task captures the
//! generation it was scheduled with and only hides if the value is
//! still current. That keeps a fast `show → show → hide_after` from
//! racing — only the latest scheduling wins.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tracing::{info, warn};

const OVERLAY_LABEL: &str = "overlay";

/// What the pill displays. Mirrors the JS-side `OverlayPhase` 1:1.
/// Externally tagged on `kind` so each variant can carry its own
/// payload without a wrapper.
///
/// `Listening` updates several times per second from the chunk pump
/// in `dispatch.rs` — `mib` is bytes shipped to CP so far, and
/// `elapsed_ms` is wall-clock since PTT down. Both are read-only
/// stats fed to the overlay pill.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayPhase {
    Listening { mib: f32, elapsed_ms: u64 },
    Transcribing,
    Done,
    Error { message: String },
}

/// Last phase emitted, cached so the overlay webview can pull it on
/// mount. Without this cache, the very first PTT press loses the
/// `Listening` event: Rust emits before the freshly-loaded webview's
/// `listen()` registration has run, so the window appears empty.
pub static LAST_PHASE: Mutex<Option<OverlayPhase>> = Mutex::new(None);

/// JS-side fetch for the cached phase. Called by `OverlayApp` on
/// mount so it can render immediately if a phase is already active.
#[tauri::command]
pub fn overlay_current_phase() -> Option<OverlayPhase> {
    LAST_PHASE.lock().expect("overlay phase mutex poisoned").clone()
}

/// Bumped on every `show` so a delayed `hide_after` task scheduled
/// by an *older* phase doesn't kill a *newer* phase. Each scheduled
/// hide captures the generation it saw and bails if the live value
/// has moved on.
static HIDE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Update the cached phase without emitting an event or touching the
/// window. Used by the chunk pump in `dispatch.rs` to publish live
/// `Listening` stats — which fire many times per second — without
/// flooding Tauri's event bus or thrashing `win.show()`. The JS
/// overlay polls `overlay_current_phase` every 100 ms, so the update
/// lands on the next tick.
pub fn update<R: Runtime>(_app: &AppHandle<R>, phase: OverlayPhase) {
    *LAST_PHASE.lock().expect("overlay phase mutex poisoned") = Some(phase);
}

/// Make the overlay visible (if it isn't already) and emit the
/// current phase. Idempotent on rapid repeat calls — Tauri's `show`
/// is a no-op when the window is already visible.
pub fn show<R: Runtime>(app: &AppHandle<R>, phase: OverlayPhase) {
    HIDE_GENERATION.fetch_add(1, Ordering::Relaxed);
    info!(?phase, "overlay: show");
    *LAST_PHASE.lock().expect("overlay phase mutex poisoned") = Some(phase.clone());
    // Broadcast rather than `emit_to(OVERLAY_LABEL, …)` — the latter
    // can race against a freshly-mounted JS listener on a webview
    // that booted from `visible: false`. The main window doesn't
    // subscribe to `overlay:phase`, so there's no collision.
    if let Err(e) = app.emit("overlay:phase", phase) {
        warn!(error = %e, "overlay: emit phase failed");
    }
    let Some(win) = app.get_webview_window(OVERLAY_LABEL) else {
        warn!("overlay: window '{OVERLAY_LABEL}' not registered — config or dev-server issue");
        return;
    };
    if let Err(e) = win.show() {
        warn!(error = %e, "overlay: show failed");
    }
}

/// Manual test entry. Invoke from the main window's devtools:
///   `await __TAURI__.core.invoke('overlay_test')`
/// Shows the pill in `Listening` phase and auto-hides after 2 s.
/// Useful when PTT itself is fine but the overlay never appears —
/// confirms whether the window exists at all.
#[tauri::command]
pub fn overlay_test(app: AppHandle) {
    show(&app, OverlayPhase::Listening { mib: 0.0, elapsed_ms: 0 });
    hide_after(&app, 2_000);
}

/// Hide the overlay window now.
pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    HIDE_GENERATION.fetch_add(1, Ordering::Relaxed);
    *LAST_PHASE.lock().expect("overlay phase mutex poisoned") = None;
    let Some(win) = app.get_webview_window(OVERLAY_LABEL) else {
        return;
    };
    if let Err(e) = win.hide() {
        warn!(error = %e, "overlay: hide failed");
    }
}

/// Schedule a hide after `delay_ms`. Captures the current generation
/// so a follow-up `show` (which bumps the generation) cancels this
/// pending hide implicitly. Used after terminal phases (Done/Error)
/// so the user sees the final state for a beat before it disappears.
pub fn hide_after<R: Runtime>(app: &AppHandle<R>, delay_ms: u64) {
    let gen_at_schedule = HIDE_GENERATION.load(Ordering::Relaxed);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        if HIDE_GENERATION.load(Ordering::Relaxed) != gen_at_schedule {
            return; // Superseded by a newer show/hide; let it own the window.
        }
        *LAST_PHASE.lock().expect("overlay phase mutex poisoned") = None;
        let Some(win) = app.get_webview_window(OVERLAY_LABEL) else {
            return;
        };
        if let Err(e) = win.hide() {
            warn!(error = %e, "overlay: deferred hide failed");
        }
    });
}
