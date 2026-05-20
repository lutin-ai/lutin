//! NVIDIA Parakeet backend, streaming via `ParakeetUnified`.
//!
//! The unified path runs the encoder over a sliding
//! (left-context + chunk + right-context) window — at default settings
//! a ~6.7 s rolling window with ~0.56 s of "new" audio per step. The
//! decoder LSTM state persists across chunks; the encoder ONNX
//! session does not, but the per-chunk encoder cost is bounded
//! regardless of total clip length. End-to-end latency from
//! end-of-speech to final transcript is therefore roughly constant
//! instead of scaling with clip duration.
//!
//! Model files live in a directory containing the istupakov ONNX
//! exports (`encoder-model.onnx` + `.data`, `decoder_joint-model.onnx`)
//! plus the SentencePiece `tokenizer.model` from mlx-community —
//! CP's download module handles the per-file repo split.

use std::path::PathBuf;

use parakeet_rs::{
    ExecutionConfig, ExecutionProvider, ParakeetUnified, ParakeetUnifiedHandle,
};

use crate::backend::{SttStream, SttStreamingFactory};
use crate::SttError;

/// Streaming factory pinned to one model directory. Loads the ORT
/// session in `load` (eagerly — desktop calls this on the warmup
/// path before the first PTT release).
pub struct ParakeetStreamingFactory {
    handle: ParakeetUnifiedHandle,
}

impl ParakeetStreamingFactory {
    pub fn load(model_dir: PathBuf) -> Result<Self, SttError> {
        let config = ExecutionConfig::new().with_execution_provider(default_provider());
        let handle = ParakeetUnifiedHandle::load(&model_dir, Some(config)).map_err(|e| {
            SttError::Load(format!(
                "load parakeet unified from {}: {e}",
                model_dir.display()
            ))
        })?;
        Ok(Self { handle })
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

impl SttStreamingFactory for ParakeetStreamingFactory {
    fn open_stream(&self) -> Result<Box<dyn SttStream>, SttError> {
        Ok(Box::new(ParakeetStream {
            inner: ParakeetUnified::from_shared(&self.handle),
        }))
    }
}

struct ParakeetStream {
    inner: ParakeetUnified,
}

impl SttStream for ParakeetStream {
    fn push(&mut self, pcm: Vec<i16>) -> Result<String, SttError> {
        let audio = pcm_i16_to_f32(&pcm);
        self.inner
            .transcribe_chunk(&audio)
            .map_err(|e| SttError::Inference(format!("parakeet streaming chunk: {e}")))
    }

    fn finish(mut self: Box<Self>) -> Result<String, SttError> {
        // Run `flush` to emit the right-context tail through the
        // chunk pipeline, then read the canonical transcript out of
        // the model's accumulator. The flush return value is just
        // the final delta — we discard it; `get_transcript` decodes
        // every token seen so far, which is what the CP wire
        // `Transcription { text }` reply expects.
        self.inner
            .flush()
            .map_err(|e| SttError::Inference(format!("parakeet streaming flush: {e}")))?;
        Ok(self.inner.get_transcript().trim().to_owned())
    }
}

fn pcm_i16_to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|s| *s as f32 / i16::MAX as f32).collect()
}
