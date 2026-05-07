//! Backend abstraction. Implementations sit behind `TtsBackendFactory`
//! and produce per-thread `TtsWorker`s; the service-level pool is
//! backend-agnostic (`service.rs`). Adding a new backend (Kokoro, a
//! cloud API, …) is one new factory + worker, no changes outside this
//! crate's call sites.

use std::sync::mpsc as std_mpsc;

use tokio::sync::watch;

use crate::TtsError;

/// A single worker's state. Each worker thread owns one instance.
/// Must be `Send` (moved to the worker thread) but not `Sync` —
/// `generate` takes `&mut self`, so the pool guarantees one thread at
/// a time.
pub trait TtsWorker: Send {
    /// Synthesize one sentence. Stream PCM chunks (24 kHz mono i16
    /// little-endian bytes) through `audio_tx`. Check `cancel_rx`
    /// periodically and return early when set. Dropping `audio_tx`
    /// (by returning) signals completion.
    ///
    /// `speed` is a 0.5–2.0 playback-rate hint already clamped by
    /// the service. Backends that don't support speed control may
    /// ignore it.
    fn generate(
        &mut self,
        text: &str,
        voice: &str,
        speed: f32,
        cancel_rx: &watch::Receiver<bool>,
        audio_tx: &std_mpsc::SyncSender<Vec<u8>>,
    ) -> Result<(), TtsError>;
}

/// Factory that owns shared model state and spins up per-worker
/// instances. Lives on the pool thread; workers borrow from it inside
/// the pool's `thread::scope`.
pub trait TtsBackendFactory: Send {
    /// Create a worker. Called once per worker thread at pool startup.
    /// `index` is the worker number (0..N).
    fn create_worker(&self, index: usize) -> Result<Box<dyn TtsWorker + '_>, TtsError>;
}
