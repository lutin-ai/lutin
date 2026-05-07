//! whisper.cpp transcription, CP-side.
//!
//! `WhisperTranscriber` keeps a `Mutex<State>` over a small
//! `Unloaded | Loaded { model, ctx }` so the model file on disk and
//! the loaded context can never disagree, and the lock is never held
//! across an `.await` — downloads + context construction happen
//! lock-free, then a short re-lock installs the result.
//!
//! Models live under `<global_config_dir>/models/whisper/`. Adding a
//! new entry to `WhisperModel` (in `lutin-control-protocol`) is the
//! only way to make a new filename trustworthy — the wire surface
//! deserializes into the closed enum so a malicious payload can't
//! pivot to arbitrary files or hosts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use lutin_control_protocol::{BeamSize, WhisperConfig, WhisperModel};
use tokio::fs;
use tracing::{info, warn};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::downloads::download_to;

/// File-on-disk + remote URL pairing for each `WhisperModel` variant.
/// Kept in CP rather than the protocol crate because the URL is a
/// CP-side concern (the desktop never downloads).
fn filename(model: WhisperModel) -> &'static str {
    match model {
        WhisperModel::LargeV3Turbo => "ggml-large-v3-turbo.bin",
        WhisperModel::DistilLargeV3 => "ggml-distil-large-v3.bin",
    }
}

fn url(model: WhisperModel) -> &'static str {
    match model {
        WhisperModel::LargeV3Turbo => {
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin"
        }
        WhisperModel::DistilLargeV3 => {
            "https://huggingface.co/distil-whisper/distil-large-v3-ggml/resolve/main/ggml-distil-large-v3.bin"
        }
    }
}

fn into_strategy(beam: BeamSize) -> SamplingStrategy {
    match beam {
        BeamSize::Greedy => SamplingStrategy::Greedy { best_of: 1 },
        BeamSize::Beam(n) => SamplingStrategy::BeamSearch {
            beam_size: n.get() as i32,
            patience: -1.0,
        },
    }
}

pub fn models_dir(global_config_dir: &Path) -> PathBuf {
    global_config_dir.join("models").join("whisper")
}

fn model_path(global_config_dir: &Path, model: WhisperModel) -> PathBuf {
    models_dir(global_config_dir).join(filename(model))
}

/// Download the model if missing, atomic-rename into place, return the
/// final path. Existing files are reused unconditionally —
/// re-validation is too slow for the hot path; corrupt files surface
/// as a context load failure later.
pub async fn ensure_model(global_config_dir: &Path, model: WhisperModel) -> Result<PathBuf> {
    let path = model_path(global_config_dir, model);
    if path.exists() {
        return Ok(path);
    }
    let dir = models_dir(global_config_dir);
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    info!(model = ?model, url = url(model), "downloading whisper model");
    download_to(url(model), &path).await?;
    info!(model = ?model, path = %path.display(), "whisper model ready");
    Ok(path)
}

/// One-shot whisper.cpp log silencer. Call exactly once at startup —
/// `whisper-rs::set_log_callback` is process-global.
pub fn install_log_callback() {
    unsafe {
        whisper_rs::set_log_callback(Some(whisper_log_noop), std::ptr::null_mut());
    }
}

unsafe extern "C" fn whisper_log_noop(
    _level: std::ffi::c_uint,
    _msg: *const std::ffi::c_char,
    _user_data: *mut std::ffi::c_void,
) {
}

enum State {
    Unloaded,
    Loaded {
        model: WhisperModel,
        ctx: Arc<WhisperContext>,
    },
}

pub struct WhisperTranscriber {
    state: Mutex<State>,
    config_dir: PathBuf,
}

impl WhisperTranscriber {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            state: Mutex::new(State::Unloaded),
            config_dir,
        }
    }

    fn current(&self) -> Option<(WhisperModel, Arc<WhisperContext>)> {
        let guard = self.state.lock().expect("whisper state poisoned");
        match &*guard {
            State::Unloaded => None,
            State::Loaded { model, ctx } => Some((*model, ctx.clone())),
        }
    }

    fn install(&self, model: WhisperModel, ctx: Arc<WhisperContext>) -> Arc<WhisperContext> {
        let mut guard = self.state.lock().expect("whisper state poisoned");
        *guard = State::Loaded {
            model,
            ctx: ctx.clone(),
        };
        ctx
    }

    pub async fn ensure_ctx(&self, model: WhisperModel) -> Result<Arc<WhisperContext>> {
        if let Some((m, ctx)) = self.current()
            && m == model
        {
            return Ok(ctx);
        }
        let path = ensure_model(&self.config_dir, model).await?;
        let ctx = tokio::task::spawn_blocking(move || -> Result<WhisperContext> {
            WhisperContext::new_with_params(
                path.to_str().unwrap_or_default(),
                WhisperContextParameters::default(),
            )
            .with_context(|| format!("load whisper model {}", path.display()))
        })
        .await
        .map_err(|e| anyhow!("whisper load task panicked: {e}"))??;
        Ok(self.install(model, Arc::new(ctx)))
    }

    pub async fn warmup(&self, model: WhisperModel) -> Result<()> {
        self.ensure_ctx(model).await.map(|_| ())
    }

    /// Run inference. `pcm` is 16 kHz mono i16 PCM. Tiny clips
    /// (< ~250 ms) short-circuit to empty so a stray PTT tap doesn't
    /// surface a hallucinated word. Conversion to f32 happens on the
    /// blocking thread so we don't pay the allocation+scaling cost on
    /// the request handler.
    pub async fn transcribe(&self, pcm: Vec<i16>, cfg: &WhisperConfig) -> Result<String> {
        const MIN_SAMPLES: usize = 4_000;
        if pcm.len() < MIN_SAMPLES {
            return Ok(String::new());
        }
        let ctx = self.ensure_ctx(cfg.model).await?;
        let language = cfg.language.clone();
        let beam = cfg.beam_size;
        let text = tokio::task::spawn_blocking(move || -> Result<String> {
            let pcm_f32: Vec<f32> = pcm.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
            let mut state: WhisperState = ctx.create_state()?;
            let mut params = FullParams::new(into_strategy(beam));
            params.set_single_segment(true);
            params.set_no_timestamps(true);
            params.set_temperature(0.0);
            params.set_no_context(true);
            params.set_suppress_nst(true);
            params.set_print_progress(false);
            params.set_print_special(false);
            params.set_print_realtime(false);
            if let Some(ref lang) = language {
                params.set_language(Some(lang.as_str()));
            }
            state.full(params, &pcm_f32)?;
            let mut out = String::with_capacity(128);
            for seg in state.as_iter() {
                out.push_str(&seg.to_str_lossy()?);
            }
            Ok(out.trim().to_owned())
        })
        .await
        .map_err(|e| anyhow!("whisper inference task panicked: {e}"))??;
        if text.is_empty() {
            warn!("whisper returned empty transcription");
        }
        Ok(text)
    }
}
