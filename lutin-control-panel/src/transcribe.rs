//! CP-side STT: model download/management plus a per-backend worker
//! cache. Inference is delegated to `lutin-stt` — this file only owns
//! the bits that depend on CP's filesystem layout and wire enums.
//!
//! Models live under `<global_config_dir>/models/<backend>/`. Adding
//! a new entry to `WhisperModel` / `ParakeetModel` (in
//! `lutin-control-protocol`) is the only way to make a new filename
//! trustworthy — the wire surface deserializes into the closed enums
//! so a malicious payload can't pivot to arbitrary files or hosts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use lutin_control_protocol::{
    BeamSize, ParakeetConfig, ParakeetModel, SttConfig, WhisperConfig, WhisperModel,
};
use lutin_stt::parakeet::ParakeetFactory;
use lutin_stt::whisper::WhisperFactory;
use lutin_stt::{SttBackendFactory, SttError, SttWorker, TranscribeParams};
use thiserror::Error;
use tokio::fs;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::downloads::download_to;

// -- whisper download -------------------------------------------------------

fn whisper_filename(model: WhisperModel) -> &'static str {
    match model {
        WhisperModel::LargeV3Turbo => "ggml-large-v3-turbo.bin",
        WhisperModel::DistilLargeV3 => "ggml-distil-large-v3.bin",
    }
}

fn whisper_url(model: WhisperModel) -> &'static str {
    match model {
        WhisperModel::LargeV3Turbo => {
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin"
        }
        WhisperModel::DistilLargeV3 => {
            "https://huggingface.co/distil-whisper/distil-large-v3-ggml/resolve/main/ggml-distil-large-v3.bin"
        }
    }
}

pub fn whisper_models_dir(global_config_dir: &Path) -> PathBuf {
    global_config_dir.join("models").join("whisper")
}

fn whisper_model_path(global_config_dir: &Path, model: WhisperModel) -> PathBuf {
    whisper_models_dir(global_config_dir).join(whisper_filename(model))
}

/// Download the whisper model if missing, atomic-rename into place,
/// return the final path. Existing files are reused unconditionally —
/// re-validation is too slow for the hot path; corrupt files surface
/// as a context load failure later.
pub async fn ensure_whisper_model(
    global_config_dir: &Path,
    model: WhisperModel,
) -> Result<PathBuf> {
    let path = whisper_model_path(global_config_dir, model);
    if path.exists() {
        return Ok(path);
    }
    let dir = whisper_models_dir(global_config_dir);
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    info!(model = ?model, url = whisper_url(model), "downloading whisper model");
    download_to(whisper_url(model), &path).await?;
    info!(model = ?model, path = %path.display(), "whisper model ready");
    Ok(path)
}

// -- parakeet download ------------------------------------------------------

fn parakeet_repo(model: ParakeetModel) -> &'static str {
    match model {
        // Community ONNX export — the upstream `nvidia/parakeet-tdt-*`
        // repos ship NeMo checkpoints, not ONNX. `istupakov`'s mirror
        // is the canonical conversion source we know parakeet-rs's
        // `from_pretrained` works against.
        ParakeetModel::Tdt06bV3 => "istupakov/parakeet-tdt-0.6b-v3-onnx",
    }
}

fn parakeet_subdir(model: ParakeetModel) -> &'static str {
    match model {
        ParakeetModel::Tdt06bV3 => "tdt-0.6b-v3",
    }
}

/// Files parakeet-rs's `ParakeetTDT::from_pretrained` looks up inside
/// the model directory. `encoder-model.onnx.data` is the external
/// weights blob — separate file because the `.onnx` graph references
/// it by relative path.
const PARAKEET_FILES: &[&str] = &[
    "encoder-model.onnx",
    "encoder-model.onnx.data",
    "decoder_joint-model.onnx",
    "vocab.txt",
];

pub fn parakeet_models_dir(global_config_dir: &Path) -> PathBuf {
    global_config_dir.join("models").join("parakeet")
}

fn parakeet_model_dir(global_config_dir: &Path, model: ParakeetModel) -> PathBuf {
    parakeet_models_dir(global_config_dir).join(parakeet_subdir(model))
}

/// Download the four Parakeet model files into a per-model directory
/// if any are missing. Returns the directory path so the factory can
/// load it. Each file is downloaded independently with the same
/// atomic-rename helper as whisper — partial sets get topped up
/// rather than redownloaded wholesale.
pub async fn ensure_parakeet_model(
    global_config_dir: &Path,
    model: ParakeetModel,
) -> Result<PathBuf> {
    let dir = parakeet_model_dir(global_config_dir, model);
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    let repo = parakeet_repo(model);
    for &file in PARAKEET_FILES {
        let dest = dir.join(file);
        if dest.exists() {
            continue;
        }
        let url = format!("https://huggingface.co/{repo}/resolve/main/{file}");
        info!(model = ?model, file, url = %url, "downloading parakeet file");
        download_to(&url, &dest).await?;
    }
    info!(model = ?model, path = %dir.display(), "parakeet model ready");
    Ok(dir)
}

// -- log silencer -----------------------------------------------------------

/// Re-export so `lib.rs` doesn't need to know which backend installed
/// the global log handler.
pub fn install_log_callback() {
    lutin_stt::whisper::install_log_callback();
}

// -- typed failure ----------------------------------------------------------

/// CP-level transcription failure. Two arms because the desktop UX
/// branches on them: `ModelUnavailable` is "we tried to download or
/// load and failed" → user might fix it (network, disk, model gone
/// from HF); `Inference` is "the model ran and rejected the audio"
/// → not user-recoverable. `lib.rs::handle_finish_transcription` maps
/// each to the matching `ApiError` variant.
#[derive(Debug, Error)]
pub enum SttFailure {
    #[error("model unavailable: {0:#}")]
    ModelUnavailable(anyhow::Error),
    #[error("inference: {0:#}")]
    Inference(anyhow::Error),
}

// -- manager ----------------------------------------------------------------

/// Per-backend worker cache. One `Option` slot per backend — the slot
/// is `None` until the first `ensure_*` succeeds; after that, holds the
/// loaded model + worker keyed by which model variant is in. Switching
/// to a different model within the same backend reloads (rare); two
/// backends coexist so swapping back is free.
///
/// Two named slots rather than a `HashMap<SttBackend, _>` because (a)
/// N=2 for the foreseeable future, (b) per-slot mutexes mean a whisper
/// warmup never blocks a parakeet load. The pattern duplicates one
/// time per backend — fine at N=2; revisit if a third backend lands.
pub struct SttManager {
    whisper: Mutex<Option<(WhisperModel, Arc<dyn SttWorker>)>>,
    parakeet: Mutex<Option<(ParakeetModel, Arc<dyn SttWorker>)>>,
    config_dir: PathBuf,
}

impl SttManager {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            whisper: Mutex::new(None),
            parakeet: Mutex::new(None),
            config_dir,
        }
    }

    async fn ensure_whisper(
        &self,
        model: WhisperModel,
    ) -> Result<Arc<dyn SttWorker>, SttFailure> {
        if let Some((m, w)) = &*self.whisper.lock().expect("whisper slot poisoned")
            && *m == model
        {
            return Ok(w.clone());
        }
        let path = ensure_whisper_model(&self.config_dir, model)
            .await
            .map_err(SttFailure::ModelUnavailable)?;
        let worker: Arc<dyn SttWorker> =
            tokio::task::spawn_blocking(move || -> Result<Arc<dyn SttWorker>, SttError> {
                Ok(Arc::from(WhisperFactory::new(path).create_worker()?))
            })
            .await
            .map_err(|e| SttFailure::ModelUnavailable(anyhow::anyhow!("load task panicked: {e}")))?
            .map_err(|e| SttFailure::ModelUnavailable(anyhow::anyhow!(e)))?;
        *self.whisper.lock().expect("whisper slot poisoned") = Some((model, worker.clone()));
        Ok(worker)
    }

    async fn ensure_parakeet(
        &self,
        model: ParakeetModel,
    ) -> Result<Arc<dyn SttWorker>, SttFailure> {
        if let Some((m, w)) = &*self.parakeet.lock().expect("parakeet slot poisoned")
            && *m == model
        {
            return Ok(w.clone());
        }
        let dir = ensure_parakeet_model(&self.config_dir, model)
            .await
            .map_err(SttFailure::ModelUnavailable)?;
        let worker: Arc<dyn SttWorker> =
            tokio::task::spawn_blocking(move || -> Result<Arc<dyn SttWorker>, SttError> {
                Ok(Arc::from(ParakeetFactory::new(dir).create_worker()?))
            })
            .await
            .map_err(|e| SttFailure::ModelUnavailable(anyhow::anyhow!("load task panicked: {e}")))?
            .map_err(|e| SttFailure::ModelUnavailable(anyhow::anyhow!(e)))?;
        *self.parakeet.lock().expect("parakeet slot poisoned") = Some((model, worker.clone()));
        Ok(worker)
    }

    pub async fn ensure_worker(&self, cfg: &SttConfig) -> Result<Arc<dyn SttWorker>, SttFailure> {
        match cfg {
            SttConfig::Whisper(WhisperConfig { model, .. }) => self.ensure_whisper(*model).await,
            SttConfig::Parakeet(ParakeetConfig { model }) => self.ensure_parakeet(*model).await,
        }
    }

    /// Run inference. `pcm` is 16 kHz mono i16 PCM. Conversion to f32
    /// + actual decode happens on the blocking thread inside the
    /// backend, so the caller pays no scaling cost on the request
    /// handler.
    pub async fn transcribe(
        &self,
        pcm: Vec<i16>,
        cfg: &SttConfig,
    ) -> Result<String, SttFailure> {
        let worker = self.ensure_worker(cfg).await?;
        let params = match cfg {
            SttConfig::Whisper(w) => TranscribeParams {
                language: w.language.clone(),
                beam_size: match w.beam_size {
                    BeamSize::Greedy => 1,
                    BeamSize::Beam(n) => n.get(),
                },
            },
            SttConfig::Parakeet(_) => TranscribeParams::default(),
        };
        let text = tokio::task::spawn_blocking(move || -> Result<String, SttError> {
            worker.transcribe(&pcm, &params, &never_cancelled())
        })
        .await
        .map_err(|e| SttFailure::Inference(anyhow::anyhow!("inference task panicked: {e}")))?
        .map_err(map_stt_error)?;
        if text.is_empty() {
            warn!("stt returned empty transcription");
        }
        Ok(text)
    }
}

/// `SttWorker::transcribe` requires a `cancel_rx` even though CP
/// doesn't currently route cancellation into the worker. One sender
/// per call (whose receiver is immediately dropped, leaving the
/// receiver permanently observing `false`) is the cheapest no-cancel
/// stub that satisfies the signature. A future
/// `CancelTranscription` mid-inference would replace this with a
/// real handle.
fn never_cancelled() -> watch::Receiver<bool> {
    let (_tx, rx) = watch::channel(false);
    rx
}

fn map_stt_error(e: SttError) -> SttFailure {
    match e {
        // `Cancelled` is impossible today (we never signal); fold into
        // `Inference` so the wire surface stays two-armed. Revisit if
        // mid-inference cancel lands.
        SttError::Inference(_) | SttError::Cancelled => SttFailure::Inference(anyhow::anyhow!(e)),
        SttError::Load(_) => SttFailure::ModelUnavailable(anyhow::anyhow!(e)),
    }
}
