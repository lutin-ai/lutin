//! SNAC ONNX decoder. Turns Orpheus audio tokens (custom_token_N)
//! into 24 kHz mono i16 PCM. Ported verbatim from the legacy engine —
//! the model contract (3-codebook decoder, frame layout) is fixed by
//! the upstream SNAC weights, not something we get to redesign.

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
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(path))
            .map_err(|e| SnacError::Ort(e.to_string()))?;

        let input_names: Vec<_> = session.inputs().iter().map(|i| i.name()).collect();
        let output_names: Vec<_> = session.outputs().iter().map(|o| o.name()).collect();
        tracing::debug!(?input_names, ?output_names, "SNAC decoder loaded");

        Ok(Self { session })
    }

    /// Decode raw audio tokens (`custom_token_N` numbers from Orpheus,
    /// already adjusted to the 0..AUDIO_TOKEN_COUNT range) into i16
    /// PCM. Processes complete groups of 7 tokens; leftover tokens
    /// are ignored — the caller is expected to flush a multiple of 7.
    pub fn decode(&mut self, tokens: &[u32]) -> Result<Vec<i16>, SnacError> {
        let num_frames = tokens.len() / TOKENS_PER_FRAME;
        if num_frames == 0 {
            return Ok(Vec::new());
        }

        let mut codes_0 = Vec::with_capacity(num_frames);
        let mut codes_1 = Vec::with_capacity(num_frames * 2);
        let mut codes_2 = Vec::with_capacity(num_frames * 4);

        for frame in 0..num_frames {
            let base = frame * TOKENS_PER_FRAME;
            // Each position in the 7-token frame has its own offset
            // (position * 4096), which we strip before feeding the
            // decoder.
            let t = |idx: usize| -> i64 {
                let raw = tokens[base + idx] as usize;
                raw.saturating_sub(idx * CODEBOOK_SIZE) as i64
            };

            // codes_0 (coarse): [i]
            // codes_1 (medium): [i+1, i+4]
            // codes_2 (fine):   [i+2, i+3, i+5, i+6]
            codes_0.push(t(0));
            codes_1.push(t(1));
            codes_1.push(t(4));
            codes_2.push(t(2));
            codes_2.push(t(3));
            codes_2.push(t(5));
            codes_2.push(t(6));
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

        let audio_view = audio_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| SnacError::Ort(e.to_string()))?;

        let pcm: Vec<i16> = audio_view
            .1
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();

        Ok(pcm)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SnacError {
    #[error("ONNX Runtime error: {0}")]
    Ort(String),
}
