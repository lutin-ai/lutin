//! Pluggable STT backends. Implementations sit behind the
//! `SttBackendFactory` / `SttWorker` traits in `backend`; one concrete
//! backend per submodule. Mirrors the `lutin-tts` shape so adding a
//! new backend (Parakeet, Canary, …) is one factory module plus one
//! match arm at the call site.

pub mod backend;
pub mod parakeet;
pub mod whisper;

pub use backend::{MIN_INFERENCE_SAMPLES, SttBackendFactory, SttWorker, TranscribeParams};

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    /// Backend couldn't load the model from disk: missing file, ORT
    /// graph rejected the bytes, whisper context construction failed.
    /// Distinct from `Inference` so the caller can decide whether
    /// retrying makes sense (it usually does — once the file is
    /// re-downloaded).
    #[error("model load error: {0}")]
    Load(String),

    /// Inference itself failed mid-call. Catch-all for the last leg
    /// of the pipeline — backend-specific causes (whisper segment
    /// decode, parakeet ORT run) flatten into this variant rather
    /// than getting their own arms; the variant is what callers
    /// branch on, the message is for the human.
    #[error("inference error: {0}")]
    Inference(String),

    #[error("operation cancelled")]
    Cancelled,
}
