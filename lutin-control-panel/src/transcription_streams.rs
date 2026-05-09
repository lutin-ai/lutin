//! Per-stream PCM accumulation for `OpenTranscription` /
//! `TranscribeChunk` / `FinishTranscription`.
//!
//! `Vec<Stream>` rather than `HashMap<StreamId, Stream>` per the
//! "prefer Vec" guideline — at most a handful of streams are live at
//! once, and `swap_remove` keeps `take()` O(1) regardless. `next_id`
//! is monotonic per CP boot.

use std::sync::Arc;
use std::sync::Mutex;

use lutin_control_protocol::{SttConfig, TranscriptionLimit, TranscriptionStreamId};

use crate::transcribe::SttManager;

/// Per-connection cap on simultaneously-open streams. Process-wide
/// enforcement (the registry is shared across connections) — a more
/// accurate per-connection bound would track owners explicitly, but
/// this is enough to stop a runaway client from exhausting CP memory.
pub const MAX_OPEN_STREAMS: usize = 32;

/// Per-chunk sample cap. 16 kHz × 10 s = 160_000 samples; the desktop
/// emits chunks at the cpal callback period (typically <100 ms), so
/// hitting this means a buggy or hostile client. Returning the
/// configured max in the error lets the desktop surface a useful
/// message instead of a generic failure.
pub const MAX_CHUNK_SAMPLES: usize = 160_000;

/// Open transcription. Buffer stays in i16 (matching the wire shape)
/// and converts to f32 only at `take()` time, right before whisper
/// inference — halves resident memory vs. converting on append.
pub struct Stream {
    pub id: TranscriptionStreamId,
    pub config: SttConfig,
    pub samples: Vec<i16>,
}

struct Inner {
    next_id: u32,
    streams: Vec<Stream>,
}

/// Shared transcription state, cloned into every connection's
/// dispatch path. The `Mutex` is fine: each op (push/find/remove) is
/// short and never spans an `.await` — actual STT work runs on
/// `spawn_blocking` with the buffer moved out.
#[derive(Clone)]
pub struct TranscriptionRegistry {
    inner: Arc<Mutex<Inner>>,
    manager: Arc<SttManager>,
}

impl TranscriptionRegistry {
    pub fn new(manager: Arc<SttManager>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                next_id: 1,
                streams: Vec::new(),
            })),
            manager,
        }
    }

    pub fn manager(&self) -> &Arc<SttManager> {
        &self.manager
    }

    /// Allocate a stream id. Fails with `TooManyStreams` if the
    /// process-wide open count would exceed `MAX_OPEN_STREAMS`.
    pub fn open(&self, config: SttConfig) -> Result<TranscriptionStreamId, TranscriptionLimit> {
        let mut inner = self.inner.lock().expect("transcription registry poisoned");
        if inner.streams.len() >= MAX_OPEN_STREAMS {
            return Err(TranscriptionLimit::TooManyStreams {
                max: MAX_OPEN_STREAMS,
            });
        }
        let id = TranscriptionStreamId(inner.next_id);
        // `wrapping_add` keeps the counter monotonic past u32::MAX
        // (practically unreachable: would need 4B PTT presses without
        // a CP restart). The `max(1)` keeps id 0 available as a
        // future "unset" sentinel if needed.
        inner.next_id = inner.next_id.wrapping_add(1).max(1);
        inner.streams.push(Stream {
            id,
            config,
            samples: Vec::new(),
        });
        Ok(id)
    }

    /// Append i16 samples to the stream. `Ok(())` on success;
    /// `Err(ChunkTooLarge)` if the request blew the per-chunk cap;
    /// `Ok(false)` semantics replaced by an explicit
    /// `Err(TranscriptionStreamNotFound)` at the caller — here we
    /// just signal "not found" with a separate `Ok` discriminant.
    pub fn append(
        &self,
        id: TranscriptionStreamId,
        samples: &[i16],
    ) -> Result<AppendOutcome, TranscriptionLimit> {
        if samples.len() > MAX_CHUNK_SAMPLES {
            return Err(TranscriptionLimit::ChunkTooLarge {
                got: samples.len(),
                max: MAX_CHUNK_SAMPLES,
            });
        }
        let mut inner = self.inner.lock().expect("transcription registry poisoned");
        let Some(stream) = inner.streams.iter_mut().find(|s| s.id == id) else {
            return Ok(AppendOutcome::StreamNotFound);
        };
        stream.samples.extend_from_slice(samples);
        Ok(AppendOutcome::Appended)
    }

    /// Remove and return the stream so the caller can move its
    /// buffer into `spawn_blocking` without holding the registry
    /// lock across the inference.
    pub fn take(&self, id: TranscriptionStreamId) -> Option<Stream> {
        let mut inner = self.inner.lock().expect("transcription registry poisoned");
        let pos = inner.streams.iter().position(|s| s.id == id)?;
        Some(inner.streams.swap_remove(pos))
    }

    /// Idempotent cancel.
    pub fn cancel(&self, id: TranscriptionStreamId) -> bool {
        self.take(id).is_some()
    }
}

/// Result of `append` apart from the `ChunkTooLarge` failure mode.
/// Splitting it keeps the principled-errors split: `TranscriptionLimit`
/// is the wire boundary failure (typed enum); "stream not found" is
/// a normal control-flow signal (caller turns it into the matching
/// wire `ApiError::TranscriptionStreamNotFound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended,
    StreamNotFound,
}
