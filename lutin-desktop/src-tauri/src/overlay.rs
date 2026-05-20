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
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, Runtime};
use tracing::{info, warn};

const OVERLAY_LABEL: &str = "overlay";

/// Distance from the top of the monitor's work area to the pill, in
/// physical pixels. Tauri's `set_position` operates in physical units
/// regardless of DPI, so we apply the monitor's scale factor below.
const TOP_OFFSET_LOGICAL: f64 = 24.0;

/// Logical width of the overlay window, kept in sync with
/// `tauri.conf.json`. We don't query `outer_size()` because the
/// reposition runs *before* `win.show()` — on the first call the
/// window hasn't been mapped yet and `outer_size()` returns zeros on
/// some backends (notably webkit2gtk), which leaves the pill's
/// top-left corner sitting at screen center instead of its true
/// center. The window is wider than the resting pill so a streaming
/// partial transcript can grow inline without resizing the window
/// mid-utterance; the pill itself stays centered in CSS.
const WIDTH_LOGICAL: f64 = 520.0;

/// What the pill displays. Mirrors the JS-side `OverlayPhase` 1:1.
/// Externally tagged on `kind` so each variant can carry its own
/// payload without a wrapper.
///
/// `Listening` updates several times per second from the chunk pump
/// in `dispatch.rs` — `mib` is bytes shipped to CP so far,
/// `elapsed_ms` is wall-clock since PTT down, and `partial` is the
/// running transcript so far (parakeet streaming only; empty for
/// whisper or before the first delta lands).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayPhase {
    Listening {
        mib: f32,
        elapsed_ms: u64,
        partial: String,
    },
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
    // Re-anchor only when the window was hidden. Repositioning a
    // visible window on every phase change causes webkit2gtk on
    // Wayland to leave stale frames behind (the prior pill ghosts at
    // the old position), and can shift the pill by a pixel or two as
    // outer_size fluctuates. Tiling WMs like i3 still need the
    // initial nudge from dead-center, which a hidden→shown transition
    // covers.
    let was_visible = win.is_visible().unwrap_or(false);
    if !was_visible {
        position_top_center(&win);
    }
    if let Err(e) = win.show() {
        warn!(error = %e, "overlay: show failed");
    }
}

/// Move the overlay window to the horizontal center of its current
/// monitor, just below the top edge. Best-effort — if monitor info
/// isn't available we leave the position alone rather than guessing.
fn position_top_center<R: Runtime>(win: &tauri::WebviewWindow<R>) {
    let monitor = match win.current_monitor() {
        Ok(Some(m)) => m,
        Ok(None) => match win.primary_monitor() {
            Ok(Some(m)) => m,
            _ => {
                warn!("overlay: no monitor info; skipping reposition");
                return;
            }
        },
        Err(e) => {
            warn!(error = %e, "overlay: current_monitor failed");
            return;
        }
    };
    let mon_pos = monitor.position();
    let mon_size = monitor.size();
    let scale = monitor.scale_factor();
    let width_px = (WIDTH_LOGICAL * scale).round() as i32;
    let top_offset_px = (TOP_OFFSET_LOGICAL * scale).round() as i32;
    let x = mon_pos.x + ((mon_size.width as i32 - width_px) / 2);
    let y = mon_pos.y + top_offset_px;
    if let Err(e) = win.set_position(PhysicalPosition::new(x, y)) {
        warn!(error = %e, "overlay: set_position failed");
    }
}

/// Manual test entry. Invoke from the main window's devtools:
///   `await __TAURI__.core.invoke('overlay_test')`
/// Shows the pill in `Listening` phase and auto-hides after 2 s.
/// Useful when PTT itself is fine but the overlay never appears —
/// confirms whether the window exists at all.
#[tauri::command]
pub fn overlay_test(app: AppHandle) {
    show(
        &app,
        OverlayPhase::Listening {
            mib: 0.0,
            elapsed_ms: 0,
            partial: "hello world".to_string(),
        },
    );
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
