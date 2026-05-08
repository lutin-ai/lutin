//! Tauri-command surface for TTS.
//!
//! Each command is a thin wrapper: validate arguments, call CP, and
//! mirror the success into the playback module so the Rust side
//! tracks what the workflow has open. The shim (slice 5) is what
//! actually exposes these to the workflow iframe; capability gating
//! also lands in slice 5. Until then these commands trust the
//! caller — chrome-internal use only.

use lutin_control_protocol::{
    ApiError, Request, Response, ResponseOk, SessionId, TtsBackend, TtsSpeed, TtsStreamId,
};
use tauri::State;
use tracing::warn;

use crate::{AppState, cp_dispatch};

/// Pre-download / pre-load weights for a backend without opening a
/// stream. Returns once CP reports the backend is ready (or fails
/// with a load/transport error). Mirrors `whisper_ensure_model`.
#[tauri::command]
pub async fn tts_ensure_backend(
    state: State<'_, AppState>,
    backend: TtsBackend,
) -> Result<(), String> {
    match cp_dispatch(&state, Request::EnsureTtsBackend { backend }).await {
        Ok(Response::Ok(ResponseOk::TtsBackendReady)) => Ok(()),
        Ok(Response::Ok(other)) => Err(format!("unexpected response: {other:?}")),
        Ok(Response::Err(e)) => Err(format!("CP error: {e}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Open a TTS stream bound to `session`. The workflow will
/// subsequently feed text via `tts_speak` and own its lifecycle until
/// `tts_close_stream`. Audio for streams whose bound session isn't
/// the chrome-active one is dropped on the playback side, so calling
/// `speak` while the workflow's iframe is in the background is a
/// silent no-op (intentional — see `tts_playback`).
#[tauri::command]
pub async fn tts_open_stream(
    state: State<'_, AppState>,
    backend: TtsBackend,
    session: SessionId,
) -> Result<TtsStreamId, String> {
    let resp = cp_dispatch(&state, Request::OpenTtsStream { backend })
        .await
        .map_err(|e| e.to_string())?;
    let stream_id = match resp {
        Response::Ok(ResponseOk::TtsStreamOpened { stream_id }) => stream_id,
        Response::Ok(other) => return Err(format!("unexpected response: {other:?}")),
        Response::Err(ApiError::TtsBackendNotReady) => {
            return Err("TtsBackendNotReady".to_owned());
        }
        Response::Err(e) => return Err(format!("CP error: {e}")),
    };
    state.tts_playback.register(stream_id, session);
    Ok(stream_id)
}

/// Speak `text` on `stream_id` at `speed`. `TtsSpeed`'s
/// `Deserialize` enforces the 0.5..=2.0× range on the JSON value at
/// the Tauri serde boundary, so by the time this body runs the speed
/// is already valid. CP queues the utterance and starts streaming
/// `Event::TtsAudio` chunks; the command returns once CP acks the
/// queue, not when synthesis ends.
#[tauri::command]
pub async fn tts_speak(
    state: State<'_, AppState>,
    stream_id: TtsStreamId,
    text: String,
    speed: TtsSpeed,
) -> Result<(), String> {
    // Speed is applied playback-side (backend-agnostic resample);
    // CP receives it too in case a future backend grows native rate
    // control, but it's a no-op there today.
    state.tts_playback.set_speed(stream_id, speed.as_f32());
    match cp_dispatch(
        &state,
        Request::SpeakTts {
            stream_id,
            text,
            speed,
        },
    )
    .await
    {
        Ok(Response::Ok(ResponseOk::TtsSpeechQueued)) => Ok(()),
        Ok(Response::Ok(other)) => Err(format!("unexpected response: {other:?}")),
        Ok(Response::Err(e)) => Err(format!("CP error: {e}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Stop in-flight synthesis and discard locally-queued audio. The
/// queue drain happens *before* the CP round-trip so the user
/// doesn't hear tail audio after pressing stop — without it,
/// already-broadcast PCM that hasn't been played yet would still
/// fire through the cpal callback.
#[tauri::command]
pub async fn tts_cancel(
    state: State<'_, AppState>,
    stream_id: TtsStreamId,
) -> Result<(), String> {
    state.tts_playback.cancel(stream_id);
    match cp_dispatch(&state, Request::CancelTts { stream_id }).await {
        Ok(Response::Ok(ResponseOk::TtsCancelled)) => Ok(()),
        Ok(Response::Ok(other)) => Err(format!("unexpected response: {other:?}")),
        Ok(Response::Err(e)) => Err(format!("CP error: {e}")),
        Err(e) => {
            warn!(error = %e, ?stream_id, "tts_cancel: CP transport error");
            Err(e.to_string())
        }
    }
}

/// Tear down the stream. Drops the playback registration regardless
/// of CP's response — if CP never acks (transport drop), holding the
/// playback slot would leak.
#[tauri::command]
pub async fn tts_close_stream(
    state: State<'_, AppState>,
    stream_id: TtsStreamId,
) -> Result<(), String> {
    let res = cp_dispatch(&state, Request::CloseTtsStream { stream_id }).await;
    state.tts_playback.unregister(stream_id);
    match res {
        Ok(Response::Ok(ResponseOk::TtsStreamClosed)) => Ok(()),
        Ok(Response::Ok(other)) => Err(format!("unexpected response: {other:?}")),
        Ok(Response::Err(e)) => Err(format!("CP error: {e}")),
        Err(e) => Err(e.to_string()),
    }
}
