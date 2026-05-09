//! Backend abstraction. Implementations sit behind `SttBackendFactory`
//! and produce `SttWorker`s; today every wired backend is one-shot
//! (whisper.cpp, Parakeet TDT — both decode the full clip in one
//! call). When a real-streaming backend lands (`ParakeetEOU`,
//! `Nemotron`) the trait can grow a sibling `transcribe_stream`
//! method then — premature today.

use tokio::sync::watch;

use crate::SttError;

/// Minimum PCM length before a clip is worth running through a
/// backend. 16 kHz × ~0.25 s; shorter than this is almost always a
/// stray PTT tap and surfaces as a hallucinated word otherwise.
/// Lives here so the threshold is one place, not one-per-backend.
pub const MIN_INFERENCE_SAMPLES: usize = 4_000;

/// Per-call parameters shared across backends. Fields a given backend
/// doesn't understand are ignored — callers shouldn't rely on
/// per-backend semantics here. CP converts from its wire-level
/// `WhisperConfig` (and, later, sibling configs).
#[derive(Debug, Clone, Default)]
pub struct TranscribeParams {
    /// ISO language code ("en", "sv", …) or `None` for autodetect.
    pub language: Option<String>,
    /// 0 / 1 = greedy; >=2 = beam width. Backends that don't beam
    /// (Parakeet TDT, etc.) ignore this.
    pub beam_size: u8,
}

/// One backend instance. `&self` (not `&mut self`) because backends
/// here can run concurrent inferences off shared model state — whisper
/// clones a fresh `WhisperState` per call from a shared `Arc<Ctx>`,
/// and ORT sessions are similarly re-entrant. The TTS pool serialises
/// because llama.cpp contexts aren't; STT doesn't share that
/// constraint.
pub trait SttWorker: Send + Sync {
    /// Run inference on one full clip. `pcm` is 16 kHz mono i16. Honour
    /// `cancel_rx` at coarse boundaries (per-segment is fine — STT
    /// runs are short). Empty string is a valid result for "nothing
    /// said".
    fn transcribe(
        &self,
        pcm: &[i16],
        params: &TranscribeParams,
        cancel_rx: &watch::Receiver<bool>,
    ) -> Result<String, SttError>;
}

/// Factory that owns shared model state (a loaded `WhisperContext`,
/// later a loaded ORT session) and hands out worker handles. Callers
/// download/locate the model file out-of-band — the factory takes a
/// path that already exists on disk.
pub trait SttBackendFactory: Send + Sync {
    fn create_worker(&self) -> Result<Box<dyn SttWorker>, SttError>;
}
