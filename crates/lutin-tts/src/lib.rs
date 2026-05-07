//! Pluggable TTS backends. Implementations sit behind the
//! `TtsBackendFactory` / `TtsWorker` traits in `backend`; a
//! backend-agnostic worker pool in `service` drives them with
//! stream-scoped routing. Adding a new backend is one factory module
//! plus one match arm at the call site (CP).

pub mod backend;
pub mod orpheus;
mod service;

pub use backend::{TtsBackendFactory, TtsWorker};
pub use orpheus::OrpheusFactory;
pub use service::{StreamId, TtsEvent, TtsService, DEFAULT_WORKER_COUNT};

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("llama.cpp error: {0}")]
    Llama(String),

    #[error("ONNX Runtime error: {0}")]
    Ort(String),

    #[error("tokenizer error: {0}")]
    Tokenizer(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
