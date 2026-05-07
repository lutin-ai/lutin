//! Per-stream TTS session registry.
//!
//! Mirrors `transcription_streams.rs`: small `Vec<Stream>` keyed by
//! `TtsStreamId`, monotonic id allocation per CP boot. The wire id
//! `TtsStreamId(u32)` is the *only* id space — it's widened to
//! `lutin_tts::StreamId(u64)` at the boundary into the service so
//! there's no second mapping to keep in sync.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use lutin_control_protocol::{TtsBackend, TtsLimit, TtsStreamId};
use lutin_tts::TtsService;

/// Process-wide cap on simultaneously-open TTS streams. Same value as
/// the transcription side; rationale is identical (stop a buggy
/// client from exhausting CP memory before the OS does).
pub const MAX_OPEN_STREAMS: usize = 32;

pub struct Stream {
    pub id: TtsStreamId,
    pub backend: TtsBackend,
    /// Service that owns the worker pool for this stream's backend.
    /// Cloned in on `open`; the registry holds one `Arc` per stream
    /// so a backend stays alive as long as any stream points at it.
    pub service: Arc<TtsService>,
}

struct Inner {
    /// `NonZeroU32` so the wrap-and-skip-zero invariant lives in the
    /// type rather than a `.max(1)` trick on every allocation. Wire
    /// ids stay non-zero, leaving 0 free as an "unset" sentinel if
    /// future protocol code wants one.
    next_id: NonZeroU32,
    streams: Vec<Stream>,
}

#[derive(Clone)]
pub struct TtsStreamRegistry {
    inner: Arc<Mutex<Inner>>,
}

impl TtsStreamRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                next_id: NonZeroU32::new(1).expect("1 != 0"),
                streams: Vec::new(),
            })),
        }
    }

    /// Allocate a wire id and bind it to `service`. Caller is
    /// responsible for resolving `backend` to the matching loaded
    /// service first (via `TtsBackends::lookup`).
    pub fn open(
        &self,
        backend: TtsBackend,
        service: Arc<TtsService>,
    ) -> Result<TtsStreamId, TtsLimit> {
        let mut inner = self.inner.lock().expect("tts streams poisoned");
        if inner.streams.len() >= MAX_OPEN_STREAMS {
            return Err(TtsLimit::TooManyStreams {
                max: MAX_OPEN_STREAMS,
            });
        }
        let id = TtsStreamId(inner.next_id.get());
        // Wrap past `u32::MAX` lands on `1`, not `0`, keeping the
        // `NonZeroU32` invariant. `MAX_OPEN_STREAMS` is 32 and ids
        // never get reused while live, so the once-per-2³² wrap is
        // comfortably outside any realistic open-set.
        inner.next_id = inner
            .next_id
            .checked_add(1)
            .unwrap_or(NonZeroU32::new(1).expect("1 != 0"));
        inner.streams.push(Stream {
            id,
            backend,
            service,
        });
        Ok(id)
    }

    /// Resolve `id` to its service plus a clone of the bound
    /// `TtsBackend` (so the caller can read voice/model without
    /// re-walking the registry). Returns `None` if the stream was
    /// never opened or has been closed — caller maps that to
    /// `ApiError::TtsStreamNotFound`.
    pub fn lookup(&self, id: TtsStreamId) -> Option<(Arc<TtsService>, TtsBackend)> {
        let inner = self.inner.lock().expect("tts streams poisoned");
        inner
            .streams
            .iter()
            .find(|s| s.id == id)
            .map(|s| (s.service.clone(), s.backend.clone()))
    }

    /// Drop the stream, returning whether it had been registered.
    /// Idempotent — `Cancel`/`Close` on a missing id is harmless.
    pub fn take(&self, id: TtsStreamId) -> Option<Stream> {
        let mut inner = self.inner.lock().expect("tts streams poisoned");
        let pos = inner.streams.iter().position(|s| s.id == id)?;
        Some(inner.streams.swap_remove(pos))
    }
}

impl Default for TtsStreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}
