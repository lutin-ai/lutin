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
use lutin_stt::parakeet::ParakeetStreamingFactory;
use lutin_stt::whisper::WhisperFactory;
use lutin_stt::{
    SttBackendFactory, SttError, SttStream, SttStreamingFactory, SttWorker, TranscribeParams,
};
use thiserror::Error;
use tokio::fs;
use tokio::sync::{watch, Mutex as AsyncMutex};
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

fn parakeet_subdir(model: ParakeetModel) -> &'static str {
    match model {
        // Wire enum still says `Tdt06bV3` for back-compat with old
        // settings files, but on disk we keep the streaming-unified
        // weights under their own subdir — the file names and the
        // model itself are different from istupakov's offline export,
        // and reusing the old dir would mix incompatible weights.
        ParakeetModel::Tdt06bV3 => "unified-0.6b-en",
    }
}

/// Files `parakeet-rs`'s `ParakeetUnified::from_pretrained` looks up
/// inside the model directory. We point at `bobNight`'s ONNX mirror
/// of `nvidia/parakeet-unified-en-0.6b` — the *streaming-trained*
/// checkpoint, not the offline TDT v3 export. The streaming code
/// path runs the encoder on overlapping windows; the offline model
/// produces only blank tokens when fed those windows, hence the
/// switch. The repo ships everything at root with the exact file
/// names the loader looks for. `encoder.onnx.data` is the external
/// weights blob the `.onnx` graph references by relative path.
fn parakeet_files(model: ParakeetModel) -> &'static [(&'static str, &'static str)] {
    match model {
        ParakeetModel::Tdt06bV3 => &[
            ("encoder.onnx", "bobNight/parakeet-unified-en-0.6b-onnx"),
            ("encoder.onnx.data", "bobNight/parakeet-unified-en-0.6b-onnx"),
            ("decoder_joint.onnx", "bobNight/parakeet-unified-en-0.6b-onnx"),
            ("tokenizer.model", "bobNight/parakeet-unified-en-0.6b-onnx"),
        ],
    }
}

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
    for &(file, repo) in parakeet_files(model) {
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
    parakeet: Mutex<Option<(ParakeetModel, Arc<dyn SttStreamingFactory>)>>,
    /// Serialises whisper / parakeet model loads so two concurrent
    /// `ensure_*` calls don't both fall through the cache check and
    /// race two `commit_from_file` invocations against the same
    /// ONNX file. ORT 1.24 has been observed to surface a bare
    /// `io::Error("No such file or directory")` when a second
    /// session-build runs against a model whose external data file
    /// is still being mapped by the first; serialising here makes
    /// the second caller a fast cache hit.
    whisper_load: AsyncMutex<()>,
    parakeet_load: AsyncMutex<()>,
    config_dir: PathBuf,
}

impl SttManager {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            whisper: Mutex::new(None),
            parakeet: Mutex::new(None),
            whisper_load: AsyncMutex::new(()),
            parakeet_load: AsyncMutex::new(()),
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
        let _load_guard = self.whisper_load.lock().await;
        // Recheck under the load gate: a concurrent caller may have
        // populated the slot while we were waiting.
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
    ) -> Result<Arc<dyn SttStreamingFactory>, SttFailure> {
        if let Some((m, f)) = &*self.parakeet.lock().expect("parakeet slot poisoned")
            && *m == model
        {
            return Ok(f.clone());
        }
        let _load_guard = self.parakeet_load.lock().await;
        // Recheck under the load gate (see [`ensure_whisper`]).
        if let Some((m, f)) = &*self.parakeet.lock().expect("parakeet slot poisoned")
            && *m == model
        {
            return Ok(f.clone());
        }
        let dir = ensure_parakeet_model(&self.config_dir, model)
            .await
            .map_err(SttFailure::ModelUnavailable)?;
        let factory: Arc<dyn SttStreamingFactory> = tokio::task::spawn_blocking(
            move || -> Result<Arc<dyn SttStreamingFactory>, SttError> {
                Ok(Arc::new(ParakeetStreamingFactory::load(dir)?))
            },
        )
        .await
        .map_err(|e| SttFailure::ModelUnavailable(anyhow::anyhow!("load task panicked: {e}")))?
        .map_err(|e| SttFailure::ModelUnavailable(anyhow::anyhow!(e)))?;
        *self.parakeet.lock().expect("parakeet slot poisoned") = Some((model, factory.clone()));
        Ok(factory)
    }

    /// Warm up whichever backend the config selects. Used by the
    /// `OpenTranscription` handler to kick off the model load before
    /// the user has finished talking. Returns nothing — callers
    /// surface load errors on the next real op.
    pub async fn ensure_worker(&self, cfg: &SttConfig) -> Result<(), SttFailure> {
        match cfg {
            SttConfig::Whisper(WhisperConfig { model, .. }) => {
                self.ensure_whisper(*model).await.map(|_| ())
            }
            SttConfig::Parakeet(ParakeetConfig { model }) => {
                self.ensure_parakeet(*model).await.map(|_| ())
            }
        }
    }

    /// Run one-shot whisper inference. `pcm` is 16 kHz mono i16 PCM —
    /// conversion to f32 + decode happen on the blocking thread inside
    /// the backend. Parakeet uses `open_parakeet_stream` instead.
    pub async fn transcribe_whisper(
        &self,
        pcm: Vec<i16>,
        cfg: &WhisperConfig,
    ) -> Result<String, SttFailure> {
        let worker = self.ensure_whisper(cfg.model).await?;
        let params = TranscribeParams {
            language: cfg.language.clone(),
            beam_size: match cfg.beam_size {
                BeamSize::Greedy => 1,
                BeamSize::Beam(n) => n.get(),
            },
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

    /// Open a new parakeet streaming session. The returned
    /// `Box<dyn SttStream>` carries per-stream decoder state and is
    /// driven by the per-stream worker task in
    /// `transcription_streams`.
    pub async fn open_parakeet_stream(
        &self,
        cfg: &ParakeetConfig,
    ) -> Result<Box<dyn SttStream>, SttFailure> {
        let factory = self.ensure_parakeet(cfg.model).await?;
        tokio::task::spawn_blocking(move || factory.open_stream())
            .await
            .map_err(|e| SttFailure::Inference(anyhow::anyhow!("open stream task panicked: {e}")))?
            .map_err(map_stt_error)
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
