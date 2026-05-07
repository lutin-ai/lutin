//! Routing layer between hotkey events and the audio/transcription
//! pipeline.
//!
//! `dispatch` is the single entry point. The keybind handler calls it
//! once per OS key event after looking up the bound `Action`/`Target`.
//! Wake-word lands later as another `Trigger` variant feeding the
//! same function — routing logic doesn't care which input source
//! armed the capture.
//!
//! All targets are wired: clipboard, active workflow iframe (gated on
//! `receive_transcription`), and pinned workflow (when its session is
//! the active one). Transcription failures fall through to the
//! clipboard with an error marker so the user gets a visible signal
//! rather than silently losing audio.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tracing::{debug, error, warn};

use crate::capability;
use crate::keybind::ShortcutPhase;
use crate::overlay;
use crate::overlay::OverlayPhase;
use crate::settings::{Action, Target};
use crate::{ActiveSession, AppState};

/// Per-session payload for the `transcription:<session_id>` Tauri
/// event. The shim flattens this into the iframe MessagePort frame
/// `{ kind: "transcription", text, source }`.
#[derive(Clone, Debug, Serialize)]
struct TranscriptionEvent<'a> {
    text: &'a str,
    source: &'static str,
}

/// Source that armed a capture. Hotkey today; wake-word becomes a
/// peer variant when slice 6 lands. Routing stays uniform.
#[derive(Clone, Copy, Debug)]
pub enum Trigger {
    Hotkey,
}

/// Single dispatch entry. Runs synchronously on the tokio runtime so
/// the OS keyboard thread is never blocked. The transcription leg can
/// take a while (whisper-rs in slice 4); calling it inline on the
/// keyboard thread would freeze hotkeys system-wide.
pub fn dispatch<R: Runtime>(
    app: AppHandle<R>,
    trigger: Trigger,
    action: Action,
    target: Target,
    phase: ShortcutPhase,
) {
    let state = app.state::<AppState>();
    let tokio = state.tokio.clone();
    tokio.spawn(async move {
        let app2 = app.clone();
        let state = app2.state::<AppState>();
        match (action, phase) {
            (Action::Ptt, ShortcutPhase::Down) => {
                state.audio.start();
                overlay::show(&app2, OverlayPhase::Listening);
                debug!(?trigger, "PTT down: capture armed");
            }
            (Action::Ptt, ShortcutPhase::Up) => {
                let pcm = state.audio.stop();
                overlay::show(&app2, OverlayPhase::Transcribing);
                let cfg = state
                    .settings
                    .lock()
                    .expect("settings mutex poisoned")
                    .whisper
                    .clone();
                let text = match state.transcriber.transcribe(&pcm, &cfg).await {
                    Ok(t) => t,
                    Err(e) => {
                        error!(error = %e, "transcription failed; clipboard fallback");
                        overlay::show(
                            &app2,
                            OverlayPhase::Error { message: format!("{e}") },
                        );
                        copy_to_clipboard(&format!("[transcription failed: {e}]"));
                        overlay::hide_after(&app2, 2_000);
                        return;
                    }
                };
                if text.is_empty() {
                    debug!(samples = pcm.len(), "PTT up: empty transcription, dropping");
                    overlay::hide(&app2);
                    return;
                }
                debug!(samples = pcm.len(), text = %text, "PTT up: text ready");
                route_text(&app2, &target, &text);
                overlay::show(&app2, OverlayPhase::Done);
                overlay::hide_after(&app2, 1_800);
            }
        }
    });
}

fn route_text<R: Runtime>(app: &AppHandle<R>, target: &Target, text: &str) {
    if matches!(target, Target::Clipboard) {
        copy_to_clipboard(text);
        return;
    }
    let active = active_session(app);
    match (target, active) {
        (Target::Clipboard, _) => unreachable!("handled above"),
        (Target::ActiveWorkflow, Some(active)) => deliver_or_fallback(app, &active, text),
        (Target::ActiveWorkflow, None) => {
            warn!("ActiveWorkflow target: no active session; falling back to clipboard");
            copy_to_clipboard(text);
        }
        (Target::Workflow { workflow }, Some(active)) if active.workflow == *workflow => {
            deliver_or_fallback(app, &active, text)
        }
        // v1 resolution: only deliver when the *active* session is
        // running this workflow. Cross-workflow routing (e.g. dictate
        // into a chat session while the user is in a settings tab)
        // needs a per-workflow most-recent-session map; defer until a
        // real use case lands. For now clipboard keeps audio from
        // being lost.
        (Target::Workflow { workflow }, _) => {
            debug!(
                workflow = %workflow.as_str(),
                "Workflow target inactive; falling back to clipboard"
            );
            copy_to_clipboard(text);
        }
    }
}

/// Snapshot the active session and release the lock immediately. The
/// clone is cheap (a couple of strings + a small Vec) and keeps the
/// dispatch hot path lock-free past this point.
fn active_session<R: Runtime>(app: &AppHandle<R>) -> Option<ActiveSession> {
    app.state::<AppState>()
        .active_session
        .lock()
        .expect("active_session poisoned")
        .clone()
}

/// Emit `transcription:<session_id>` if the workflow declares
/// `receive_transcription`. Otherwise fall back to clipboard so the
/// user doesn't silently lose audio against a misconfigured plugin.
fn deliver_or_fallback<R: Runtime>(app: &AppHandle<R>, active: &ActiveSession, text: &str) {
    if !active
        .capabilities
        .iter()
        .any(|c| c == capability::RECEIVE_TRANSCRIPTION)
    {
        warn!(
            workflow = %active.workflow.as_str(),
            "workflow does not declare receive_transcription; clipboard fallback"
        );
        copy_to_clipboard(text);
        return;
    }
    let event = format!("transcription:{}", active.session.as_str());
    if let Err(e) = app.emit(&event, TranscriptionEvent { text, source: "ptt" }) {
        warn!(error = %e, event = %event, "emit transcription event failed");
    }
}

fn copy_to_clipboard(text: &str) {
    match arboard::Clipboard::new() {
        Ok(mut cb) => {
            if let Err(e) = cb.set_text(text) {
                warn!(error = %e, "clipboard set_text failed");
            }
        }
        Err(e) => warn!(error = %e, "clipboard open failed"),
    }
}
