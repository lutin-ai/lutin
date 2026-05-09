//! NVIDIA Parakeet TDT backend (multilingual).
//!
//! Wraps `parakeet-rs` (ORT under the hood) behind the `SttWorker`
//! trait. The factory takes a directory containing
//! `encoder-model.onnx`, `decoder_joint-model.onnx`, and `vocab.txt`
//! (CP downloads them out-of-band, mirroring the whisper backend).
//!
//! `parakeet-rs::Transcriber::transcribe_samples` takes `&mut self`,
//! so the worker holds the model behind a `Mutex` to satisfy the
//! crate-wide `&self` trait shape. Concurrent calls serialise — fine
//! for our PTT pattern (one clip at a time) and matches Parakeet's
//! own model-thread expectation.

use std::path::PathBuf;
use std::sync::Mutex;

use parakeet_rs::{ExecutionConfig, ExecutionProvider, ParakeetTDT, Transcriber};
use tokio::sync::watch;

use crate::backend::{MIN_INFERENCE_SAMPLES, SttBackendFactory, SttWorker, TranscribeParams};
use crate::SttError;

/// Factory pinned to one model directory. Cheap to construct; the
/// ORT sessions load on `create_worker`.
pub struct ParakeetFactory {
    model_dir: PathBuf,
    /// Execution provider chosen at build time. CUDA when the crate
    /// was compiled with `--features cuda`; CPU otherwise. (TensorRT
    /// would be the obvious win for Blackwell but parakeet-rs picks
    /// CUDA-only when both are enabled — switching to TRT means
    /// passing it through here later.)
    provider: ExecutionProvider,
}

impl ParakeetFactory {
    pub fn new(model_dir: PathBuf) -> Self {
        Self {
            model_dir,
            provider: default_provider(),
        }
    }
}

#[cfg(feature = "cuda")]
fn default_provider() -> ExecutionProvider {
    ExecutionProvider::Cuda
}

#[cfg(not(feature = "cuda"))]
fn default_provider() -> ExecutionProvider {
    ExecutionProvider::Cpu
}

impl SttBackendFactory for ParakeetFactory {
    fn create_worker(&self) -> Result<Box<dyn SttWorker>, SttError> {
        let config = ExecutionConfig::new().with_execution_provider(self.provider);
        let model = ParakeetTDT::from_pretrained(&self.model_dir, Some(config)).map_err(|e| {
            SttError::Load(format!(
                "load parakeet from {}: {e}",
                self.model_dir.display()
            ))
        })?;
        Ok(Box::new(ParakeetWorker {
            model: Mutex::new(model),
        }))
    }
}

pub struct ParakeetWorker {
    model: Mutex<ParakeetTDT>,
}

impl SttWorker for ParakeetWorker {
    fn transcribe(
        &self,
        pcm: &[i16],
        _params: &TranscribeParams,
        cancel_rx: &watch::Receiver<bool>,
    ) -> Result<String, SttError> {
        if pcm.len() < MIN_INFERENCE_SAMPLES {
            return Ok(String::new());
        }
        if *cancel_rx.borrow() {
            return Err(SttError::Cancelled);
        }
        let audio: Vec<f32> = pcm.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
        let mut guard = self.model.lock().expect("parakeet model poisoned");
        let result = guard
            .transcribe_samples(audio, 16_000, 1, None)
            .map_err(|e| SttError::Inference(format!("parakeet: {e}")))?;
        Ok(result.text.trim().to_owned())
    }
}
