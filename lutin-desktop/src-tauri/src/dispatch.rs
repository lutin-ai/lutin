//! Routing layer between hotkey events and the streaming
//! transcription pipeline.
//!
//! `dispatch` is the single entry point. The keybind handler calls it
//! once per OS key event after looking up the bound `Action`/`Target`.
//! Wake-word lands later as another `Trigger` variant feeding the
//! same function — routing logic doesn't care which input source
//! armed the capture.
//!
//! Audio is streamed to CP rather than buffered locally: PTT down
//! opens a transcription stream, each cpal callback pushes a chunk
//! up the CP WS as a `TranscribeChunk`, PTT up sends
//! `FinishTranscription` and routes the returned text. CP runs
//! whisper; the desktop's job is just the audio plumbing and the
//! user-facing target routing.

use std::time::Instant;

use lutin_control_protocol::{
    MonoPcm16k, Request, Response, ResponseOk, TranscriptionStreamId,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, error, warn};

use crate::capability;
use crate::cp_dispatch;
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

/// In-flight PTT bookkeeping. Stored in `AppState.active_ptt`.
/// `target` is captured at down-time so a release event uses the
/// binding that was active when the user started talking, even if
/// the keymap changed mid-press (improbable but defensible).
pub struct ActivePtt {
    pub stream_id: TranscriptionStreamId,
    pub target: Target,
}

/// Single dispatch entry. Runs on the tokio runtime so the OS
/// keyboard thread is never blocked.
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
        match (action, phase) {
            (Action::Ptt, ShortcutPhase::Down) => {
                ptt_down(app, trigger, target).await;
            }
            (Action::Ptt, ShortcutPhase::Up) => {
                ptt_up(app).await;
            }
        }
    });
}

async fn ptt_down<R: Runtime>(app: AppHandle<R>, trigger: Trigger, target: Target) {
    let state = app.state::<AppState>();

    // If a prior PTT is still in flight (re-press without a release),
    // cancel its CP-side stream so we don't leak the buffer for the
    // rest of the session. The keybind layer normally serialises
    // down/up pairs, so this is a defensive cleanup, not a hot path.
    let prior = state.active_ptt.lock().expect("active_ptt poisoned").take();
    if let Some(prior) = prior {
        warn!(
            stream_id = ?prior.stream_id,
            "PTT down arrived while a stream was still open; cancelling prior stream",
        );
        let _ = cp_dispatch(
            &state,
            Request::CancelTranscription { stream_id: prior.stream_id },
        )
        .await;
    }

    // Snapshot whisper config from settings. Cheap clone — sent once
    // in OpenTranscription, reused by CP for the full stream.
    let cfg = state
        .settings
        .lock()
        .expect("settings mutex poisoned")
        .whisper
        .clone();

    let Some(rx) = state.audio.start() else {
        warn!("PTT down: no mic available, dropping");
        return;
    };

    let started_at = Instant::now();
    overlay::show(&app, OverlayPhase::Listening { mib: 0.0, elapsed_ms: 0 });

    // Open the CP-side stream. Block here on the OpenTranscription
    // round-trip — until CP gives us a stream id we can't pump
    // chunks. Typically <10ms over LAN.
    let resp = match cp_dispatch(&state, Request::OpenTranscription { config: cfg }).await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "OpenTranscription failed; aborting PTT");
            state.audio.stop();
            overlay::show(&app, OverlayPhase::Error { message: e.to_string() });
            overlay::hide_after(&app, 2_000);
            return;
        }
    };
    let stream_id = match resp {
        Response::Ok(ResponseOk::TranscriptionOpened { stream_id }) => stream_id,
        Response::Ok(other) => {
            error!(?other, "OpenTranscription: unexpected response");
            state.audio.stop();
            overlay::hide(&app);
            return;
        }
        Response::Err(e) => {
            error!(error = %e, "OpenTranscription: CP error");
            state.audio.stop();
            overlay::show(&app, OverlayPhase::Error { message: e.to_string() });
            overlay::hide_after(&app, 2_000);
            return;
        }
    };

    *state.active_ptt.lock().expect("active_ptt poisoned") = Some(ActivePtt {
        stream_id,
        target,
    });

    debug!(?trigger, ?stream_id, "PTT down: stream opened");

    // Spawn the chunk pump. It owns the receiver and runs until
    // `Capture::stop()` closes it (PTT up) or CP returns an error on
    // a chunk (which we surface via the overlay and abort).
    let app_clone = app.clone();
    state.tokio.spawn(async move {
        pump_chunks(app_clone, rx, stream_id, started_at).await;
    });
}

async fn pump_chunks<R: Runtime>(
    app: AppHandle<R>,
    mut rx: UnboundedReceiver<MonoPcm16k>,
    stream_id: TranscriptionStreamId,
    started_at: Instant,
) {
    let state = app.state::<AppState>();
    let mut bytes_sent: u64 = 0;
    while let Some(samples) = rx.recv().await {
        bytes_sent = bytes_sent.saturating_add((samples.len() * 2) as u64);
        // Update the overlay live — MiB sent + elapsed listening time.
        // `update` (rather than `show`) skips the event emit + window
        // show; the overlay JS polls the cached phase 10x/sec so a
        // higher cadence here would just be wasted work.
        let mib = bytes_to_mib(bytes_sent);
        let elapsed_ms = started_at.elapsed().as_millis() as u64;
        overlay::update(
            &app,
            OverlayPhase::Listening {
                mib,
                elapsed_ms,
            },
        );

        match cp_dispatch(
            &state,
            Request::TranscribeChunk {
                stream_id,
                samples,
            },
        )
        .await
        {
            Ok(Response::Ok(ResponseOk::ChunkAccepted)) => {}
            Ok(other) => {
                warn!(?other, "TranscribeChunk: unexpected response; stopping pump");
                break;
            }
            Err(e) => {
                warn!(error = %e, "TranscribeChunk send failed; stopping pump");
                break;
            }
        }
    }
    debug!(?stream_id, mib = bytes_to_mib(bytes_sent), "chunk pump exited");
}

fn bytes_to_mib(bytes: u64) -> f32 {
    bytes as f32 / (1024.0 * 1024.0)
}

async fn ptt_up<R: Runtime>(app: AppHandle<R>) {
    let state = app.state::<AppState>();
    state.audio.stop();
    let Some(active) = state
        .active_ptt
        .lock()
        .expect("active_ptt poisoned")
        .take()
    else {
        debug!("PTT up: no active stream");
        return;
    };

    overlay::show(&app, OverlayPhase::Transcribing);

    let resp = match cp_dispatch(
        &state,
        Request::FinishTranscription {
            stream_id: active.stream_id,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "FinishTranscription failed");
            overlay::show(&app, OverlayPhase::Error { message: e.to_string() });
            overlay::hide_after(&app, 2_000);
            return;
        }
    };
    let text = match resp {
        Response::Ok(ResponseOk::Transcription { text }) => text,
        Response::Ok(other) => {
            error!(?other, "FinishTranscription: unexpected response");
            overlay::hide(&app);
            return;
        }
        Response::Err(e) => {
            error!(error = %e, "FinishTranscription: CP error");
            let msg = e.to_string();
            copy_to_clipboard(&format!("[transcription failed: {msg}]"));
            overlay::show(&app, OverlayPhase::Error { message: msg });
            overlay::hide_after(&app, 2_000);
            return;
        }
    };
    if text.is_empty() {
        debug!("PTT up: empty transcription, dropping");
        overlay::hide(&app);
        return;
    }
    debug!(text = %text, "PTT up: text ready");
    route_text(&app, &active.target, &text);
    overlay::show(&app, OverlayPhase::Done);
    overlay::hide_after(&app, 1_800);
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

fn active_session<R: Runtime>(app: &AppHandle<R>) -> Option<ActiveSession> {
    app.state::<AppState>()
        .active_session
        .lock()
        .expect("active_session poisoned")
        .clone()
}

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

/// Copy `text` to the system clipboard.
///
/// On X11/Wayland the clipboard's contents are served on demand by
/// whatever process owns the selection. Setting the text and dropping
/// the `Clipboard` immediately releases ownership before any consumer
/// (including the clipboard manager) has a chance to pull the data,
/// which is exactly the "wrote it but paste yields nothing" symptom.
///
/// `SetExtLinux::wait()` keeps the calling thread alive serving
/// selection requests until another app overwrites the clipboard, so
/// we spawn a dedicated OS thread per copy. The thread exits when
/// ownership is taken back; no cleanup needed on our end.
fn copy_to_clipboard(text: &str) {
    let owned = text.to_owned();
    std::thread::Builder::new()
        .name("clipboard-owner".into())
        .spawn(move || {
            use arboard::SetExtLinux;
            let mut cb = match arboard::Clipboard::new() {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "clipboard open failed");
                    return;
                }
            };
            if let Err(e) = cb.set().wait().text(owned) {
                warn!(error = %e, "clipboard set_text failed");
            }
        })
        .expect("spawn clipboard-owner thread");
}
