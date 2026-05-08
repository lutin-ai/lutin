//! SNAC ONNX decoder. Turns Orpheus audio tokens (custom_token_N)
//! into 24 kHz mono i16 PCM. The decoder is convolutional with a
//! receptive field that spans several frames; streaming requires
//! feeding the previous tail back in as context, otherwise samples
//! near each chunk boundary are computed without their full input
//! support and audible discontinuities (clicks, perceived pitch
//! wobble) appear at every chunk join.

use std::path::Path;

use ort::session::Session;

/// Number of tokens per SNAC frame group. Re-exported via the
/// `orpheus` parent so the audio-token range and the decoder both
/// derive from one definition.
pub(super) const TOKENS_PER_FRAME: usize = 7;

/// Codebook size per layer. Same single-source-of-truth concern.
pub(super) const CODEBOOK_SIZE: usize = 4096;

pub struct SnacDecoder {
    session: Session,
}

impl SnacDecoder {
    pub fn from_file(path: &Path) -> Result<Self, SnacError> {
        let mut builder =
            Session::builder().map_err(|e| SnacError::Ort(e.to_string()))?;

        // Register GPU execution providers when compiled in. TensorRT
        // is preferred — it fuses the SNAC graph and JITs kernels for
        // the local GPU, which matters on Blackwell where the prebuilt
        // CUDA EP from microsoft/pyke ships no sm_120 SASS. CUDA is
        // listed second as a fallback for any op TRT doesn't cover; if
        // both fail to register, ORT falls back to CPU and we still get
        // audio on a misconfigured host.
        //
        // Engine cache: TRT serializes a per-GPU `.engine` blob the first
        // time it sees the model (build takes seconds), then reuses it.
        // Cache is keyed by GPU + TRT version internally.
        #[cfg(feature = "cuda")]
        {
            use ort::execution_providers::{CUDAExecutionProvider, TensorRTExecutionProvider};
            let cache_dir = trt_engine_cache_dir();
            if let Err(e) = std::fs::create_dir_all(&cache_dir) {
                tracing::warn!(?cache_dir, error = %e, "could not create TRT engine cache dir");
            }
            builder = builder
                .with_execution_providers([
                    TensorRTExecutionProvider::default()
                        .with_device_id(0)
                        .with_fp16(true)
                        .with_engine_cache(true)
                        .with_engine_cache_path(cache_dir.to_string_lossy().into_owned())
                        .with_timing_cache(true)
                        .build(),
                    CUDAExecutionProvider::default().build(),
                ])
                .map_err(|e| SnacError::Ort(e.to_string()))?;
        }

        let session = builder
            .commit_from_file(path)
            .map_err(|e| SnacError::Ort(e.to_string()))?;

        let input_names: Vec<_> = session.inputs().iter().map(|i| i.name()).collect();
        let output_names: Vec<_> = session.outputs().iter().map(|o| o.name()).collect();
        tracing::debug!(?input_names, ?output_names, "SNAC decoder loaded");

        Ok(Self { session })
    }

    /// Decode `lookback` followed by `new_tokens` and return only the
    /// PCM samples that correspond to `new_tokens`. The lookback gives
    /// the convolutional decoder the past context it needs so the
    /// first samples of the returned slice are computed with the same
    /// receptive support as samples in the middle — no boundary
    /// glitch.
    ///
    /// `lookback` and `new_tokens` are both flat token streams whose
    /// length must be a multiple of `TOKENS_PER_FRAME`. Pass an empty
    /// `lookback` for the first chunk of an utterance.
    pub fn decode(
        &mut self,
        lookback: &[u32],
        new_tokens: &[u32],
    ) -> Result<Vec<i16>, SnacError> {
        let lookback_frames = lookback.len() / TOKENS_PER_FRAME;
        let new_frames = new_tokens.len() / TOKENS_PER_FRAME;
        let num_frames = lookback_frames + new_frames;
        if new_frames == 0 {
            return Ok(Vec::new());
        }

        let mut codes_0 = Vec::with_capacity(num_frames);
        let mut codes_1 = Vec::with_capacity(num_frames * 2);
        let mut codes_2 = Vec::with_capacity(num_frames * 4);

        let push_frame = |codes_0: &mut Vec<i64>,
                          codes_1: &mut Vec<i64>,
                          codes_2: &mut Vec<i64>,
                          frame: &[u32]| {
            // Each position in the 7-token frame has its own offset
            // (position * 4096), which we strip before feeding the
            // decoder.
            let t = |idx: usize| -> i64 {
                let raw = frame[idx] as usize;
                raw.saturating_sub(idx * CODEBOOK_SIZE) as i64
            };
            // codes_0 (coarse): [0]
            // codes_1 (medium): [1, 4]
            // codes_2 (fine):   [2, 3, 5, 6]
            codes_0.push(t(0));
            codes_1.push(t(1));
            codes_1.push(t(4));
            codes_2.push(t(2));
            codes_2.push(t(3));
            codes_2.push(t(5));
            codes_2.push(t(6));
        };

        for f in 0..lookback_frames {
            let base = f * TOKENS_PER_FRAME;
            push_frame(
                &mut codes_0,
                &mut codes_1,
                &mut codes_2,
                &lookback[base..base + TOKENS_PER_FRAME],
            );
        }
        for f in 0..new_frames {
            let base = f * TOKENS_PER_FRAME;
            push_frame(
                &mut codes_0,
                &mut codes_1,
                &mut codes_2,
                &new_tokens[base..base + TOKENS_PER_FRAME],
            );
        }

        let shape_0: [usize; 2] = [1, codes_0.len()];
        let shape_1: [usize; 2] = [1, codes_1.len()];
        let shape_2: [usize; 2] = [1, codes_2.len()];

        let t0 = ort::value::Tensor::from_array((shape_0, codes_0.into_boxed_slice()))
            .map_err(|e| SnacError::Ort(e.to_string()))?;
        let t1 = ort::value::Tensor::from_array((shape_1, codes_1.into_boxed_slice()))
            .map_err(|e| SnacError::Ort(e.to_string()))?;
        let t2 = ort::value::Tensor::from_array((shape_2, codes_2.into_boxed_slice()))
            .map_err(|e| SnacError::Ort(e.to_string()))?;

        let inputs = ort::inputs![
            "audio_codes.0" => t0,
            "audio_codes.1" => t1,
            "audio_codes.2" => t2,
        ];

        let outputs = self
            .session
            .run(inputs)
            .map_err(|e| SnacError::Ort(e.to_string()))?;

        // Output: [1, 1, num_samples] of f32 in [-1, 1].
        let audio_tensor = outputs
            .values()
            .next()
            .ok_or_else(|| SnacError::Ort("no output tensor".into()))?;

        let (_shape, samples) = audio_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| SnacError::Ort(e.to_string()))?;

        // Derive samples-per-frame from the actual decoder output
        // rather than hard-coding it; SNAC variants differ in stride.
        let total = samples.len();
        if total % num_frames != 0 {
            return Err(SnacError::Ort(format!(
                "SNAC output {} not divisible by {} frames",
                total, num_frames
            )));
        }
        let samples_per_frame = total / num_frames;
        let trim = lookback_frames * samples_per_frame;

        let pcm: Vec<i16> = samples[trim..]
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();

        Ok(pcm)
    }
}

#[cfg(feature = "cuda")]
fn trt_engine_cache_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join("lutin").join("trt-engines")
}

#[derive(Debug, thiserror::Error)]
pub enum SnacError {
    #[error("ONNX Runtime error: {0}")]
    Ort(String),
}
