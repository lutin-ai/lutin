//! Orpheus TTS backend. Llama.cpp generates audio tokens; SNAC ONNX
//! decodes them to 24 kHz mono i16 PCM. Ported from the legacy engine
//! with cosmetic changes only (tracing instead of log).

mod snac;

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use tokio::sync::watch;

use crate::backend::{TtsBackendFactory, TtsWorker};
use crate::TtsError;
use snac::{SnacDecoder, CODEBOOK_SIZE, TOKENS_PER_FRAME};

/// Maximum tokens to generate per sentence.
const MAX_TOKENS: usize = 2000;

/// `TOKENS_PER_FRAME` positions × `CODEBOOK_SIZE` entries each (each
/// position has its own offset). Derived rather than hard-coded so a
/// model-side change to either dimension can't desync silently.
const AUDIO_TOKEN_COUNT: u32 = (CODEBOOK_SIZE * TOKENS_PER_FRAME) as u32;

/// SNAC frames to accumulate before decoding and emitting a chunk.
/// 16 frames ≈ 200 ms of audio at 24 kHz — small enough to keep
/// latency low, large enough to amortise the ONNX call cost.
const STREAM_CHUNK_FRAMES: usize = 16;
const STREAM_CHUNK_TOKENS: usize = STREAM_CHUNK_FRAMES * TOKENS_PER_FRAME;

/// Shared model state. Owns the LlamaModel + backend so workers can
/// borrow them when constructing per-thread contexts inside the pool's
/// `thread::scope`.
pub struct OrpheusFactory {
    backend: LlamaBackend,
    model: LlamaModel,
    audio_token_start: u32,
    snac_path: PathBuf,
}

impl OrpheusFactory {
    /// Load the Orpheus GGUF model and detect the audio-token range.
    /// Must be called from a blocking context (heavy disk + GPU init).
    pub fn load(gguf_path: &Path, snac_path: &Path) -> Result<Self, TtsError> {
        llama_cpp_2::send_logs_to_tracing(
            llama_cpp_2::LogOptions::default().with_logs_enabled(false),
        );
        let backend =
            LlamaBackend::init().map_err(|e| TtsError::Llama(e.to_string()))?;

        let model_params = LlamaModelParams::default().with_n_gpu_layers(999);
        let model = LlamaModel::load_from_file(&backend, gguf_path, &model_params)
            .map_err(|e| TtsError::Llama(e.to_string()))?;

        let audio_token_start = find_audio_token_start(&model)?;
        tracing::info!(
            start = audio_token_start,
            end = audio_token_start + AUDIO_TOKEN_COUNT,
            "Orpheus audio token range",
        );

        Ok(Self {
            backend,
            model,
            audio_token_start,
            snac_path: snac_path.to_path_buf(),
        })
    }
}

impl TtsBackendFactory for OrpheusFactory {
    fn create_worker(&self, index: usize) -> Result<Box<dyn TtsWorker + '_>, TtsError> {
        let ctx_params =
            LlamaContextParams::default().with_n_ctx(std::num::NonZero::new(2048));
        let ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| TtsError::Llama(e.to_string()))?;
        let snac = SnacDecoder::from_file(&self.snac_path)
            .map_err(|e| TtsError::Ort(e.to_string()))?;

        tracing::info!(worker = index, "Orpheus worker ready");

        Ok(Box::new(OrpheusWorker {
            model: &self.model,
            ctx,
            snac,
            audio_token_start: self.audio_token_start,
            audio_token_end: self.audio_token_start + AUDIO_TOKEN_COUNT,
        }))
    }
}

struct OrpheusWorker<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    snac: SnacDecoder,
    audio_token_start: u32,
    audio_token_end: u32,
}

// SAFETY: each worker is owned by exactly one thread (the pool moves
// it once at creation). LlamaContext wraps a raw pointer that's safe
// from any single thread; the `&mut self` requirement on `generate`
// prevents concurrent access.
unsafe impl Send for OrpheusWorker<'_> {}

impl TtsWorker for OrpheusWorker<'_> {
    fn generate(
        &mut self,
        text: &str,
        voice: &str,
        speed: f32,
        cancel_rx: &watch::Receiver<bool>,
        audio_tx: &std_mpsc::SyncSender<Vec<u8>>,
    ) -> Result<(), TtsError> {
        // Orpheus has no in-model speed control; SNAC output rate is
        // fixed at 24 kHz. Resampling at the output stage would cost
        // a perceptible quality hit, so we deliberately ignore the
        // hint here. Documented at the trait level (`backend.rs`).
        let _ = speed;
        let prompt = format!("<|audio|><{voice}>: {text}<|eot_id|>");

        let tokens = self
            .model
            .str_to_token(&prompt, llama_cpp_2::model::AddBos::Always)
            .map_err(|e| TtsError::Llama(e.to_string()))?;

        self.ctx.clear_kv_cache();

        let mut batch =
            llama_cpp_2::llama_batch::LlamaBatch::new(tokens.len() + MAX_TOKENS, 1);
        for (i, &token) in tokens.iter().enumerate() {
            let is_last = i == tokens.len() - 1;
            batch
                .add(token, i as i32, &[0], is_last)
                .map_err(|_| TtsError::Llama("failed to add token to batch".into()))?;
        }

        self.ctx
            .decode(&mut batch)
            .map_err(|e| TtsError::Llama(e.to_string()))?;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(0.6),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::dist(0),
        ]);

        let mut audio_tokens: Vec<u32> = Vec::new();
        let mut n_generated = 0;

        loop {
            if *cancel_rx.borrow() {
                break;
            }

            let token = sampler.sample(&self.ctx, batch.n_tokens() - 1);

            if self.model.is_eog_token(token) {
                break;
            }

            let token_id = token.0 as u32;

            if token_id >= self.audio_token_start && token_id < self.audio_token_end {
                audio_tokens.push(token_id - self.audio_token_start);

                if audio_tokens.len() >= STREAM_CHUNK_TOKENS {
                    let usable =
                        (audio_tokens.len() / TOKENS_PER_FRAME) * TOKENS_PER_FRAME;
                    send_chunk(&mut self.snac, &audio_tokens[..usable], audio_tx);
                    audio_tokens.drain(..usable);
                }
            }

            n_generated += 1;
            if n_generated >= MAX_TOKENS {
                break;
            }

            batch.clear();
            batch
                .add(
                    token,
                    (tokens.len() + n_generated - 1) as i32,
                    &[0],
                    true,
                )
                .map_err(|_| TtsError::Llama("failed to add token to batch".into()))?;

            self.ctx
                .decode(&mut batch)
                .map_err(|e| TtsError::Llama(e.to_string()))?;
        }

        // Flush remaining tokens (rounded down to the nearest frame).
        let usable = (audio_tokens.len() / TOKENS_PER_FRAME) * TOKENS_PER_FRAME;
        if usable >= TOKENS_PER_FRAME {
            send_chunk(&mut self.snac, &audio_tokens[..usable], audio_tx);
        }

        Ok(())
    }
}

fn send_chunk(
    snac: &mut SnacDecoder,
    tokens: &[u32],
    audio_tx: &std_mpsc::SyncSender<Vec<u8>>,
) {
    match snac.decode(tokens) {
        Ok(pcm) if !pcm.is_empty() => {
            let data: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
            let _ = audio_tx.send(data);
        }
        Err(e) => tracing::warn!(error = %e, "SNAC decode error"),
        _ => {}
    }
}

/// Find the first audio-token id. Orpheus reserves custom tokens 0–9
/// for special use; audio codes start at 10. The direct `str_to_token`
/// lookup is the contract for current Orpheus GGUFs (Q4_K_M and the
/// official 0.1-ft set). The vocab scan is a fallback for older or
/// re-quantised exports whose tokeniser doesn't surface
/// `<custom_token_10>` as a single piece — it warns when it fires so
/// we can tell whether the fallback is still load-bearing.
fn find_audio_token_start(model: &LlamaModel) -> Result<u32, TtsError> {
    if let Ok(ids) =
        model.str_to_token("<custom_token_10>", llama_cpp_2::model::AddBos::Never)
    {
        if let Some(&tok) = ids.first() {
            return Ok(tok.0 as u32);
        }
    }

    tracing::warn!("Orpheus: direct <custom_token_10> lookup failed, falling back to vocab scan");
    let n_vocab = model.n_vocab();
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    for id in 0..n_vocab {
        if let Ok(s) = model.token_to_piece(LlamaToken(id), &mut decoder, true, None) {
            if s.contains("custom_token_10") {
                return Ok(id as u32);
            }
        }
    }

    Err(TtsError::Llama(
        "could not find audio token range in model vocabulary".into(),
    ))
}
