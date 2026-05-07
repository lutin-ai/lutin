//! TTS model fetch + backend registry, CP-side.
//!
//! `TtsBackends` is a process-wide cache of loaded
//! [`lutin_tts::TtsService`]s, keyed by what determines the
//! weights-on-GPU identity (`BackendKey`). Per-utterance config (voice,
//! speed) is *not* part of the key — two streams with different voices
//! share one loaded model.
//!
//! `EnsureTtsBackend` is the only entry point that triggers a download
//! / factory load; `OpenTtsStream` is fast-path lookup that returns
//! `TtsBackendNotReady` on miss. The split keeps the open path
//! predictable and gives the UI a single place to surface progress.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use lutin_control_protocol::{OrpheusModel, OrpheusVoice, TtsBackend};
use lutin_tts::{DEFAULT_WORKER_COUNT, OrpheusFactory, TtsEvent, TtsService};
use tokio::fs;
use tokio::sync::mpsc;
use tracing::info;

/// Cache key for a loaded backend. Each variant captures only the
/// fields that determine which weights are in VRAM — voice and other
/// per-utterance config are excluded. Add new variants here as new
/// backends land; never reuse one across backends.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum BackendKey {
    Orpheus(OrpheusModel),
}

impl BackendKey {
    fn from_backend(b: &TtsBackend) -> Self {
        match b {
            TtsBackend::Orpheus { model, .. } => Self::Orpheus(*model),
        }
    }
}

fn orpheus_filename(m: OrpheusModel) -> &'static str {
    match m {
        OrpheusModel::ThreeBQ4KM => "orpheus-3b-0.1-ft-Q4_K_M.gguf",
    }
}

fn orpheus_url(m: OrpheusModel) -> &'static str {
    match m {
        OrpheusModel::ThreeBQ4KM => {
            "https://huggingface.co/isaiahbjork/orpheus-3b-0.1-ft-Q4_K_M-GGUF/resolve/main/orpheus-3b-0.1-ft-Q4_K_M.gguf"
        }
    }
}

const SNAC_FILENAME: &str = "snac_decoder_model.onnx";
const SNAC_URL: &str =
    "https://huggingface.co/onnx-community/snac_24khz-ONNX/resolve/main/onnx/decoder_model.onnx";

/// Backend-internal voice token. Kept here at the CP boundary so the
/// wire enum is the only voice surface the rest of the system sees.
pub fn voice_token(v: OrpheusVoice) -> &'static str {
    match v {
        OrpheusVoice::Tara => "tara",
        OrpheusVoice::Leah => "leah",
        OrpheusVoice::Jess => "jess",
        OrpheusVoice::Leo => "leo",
        OrpheusVoice::Dan => "dan",
        OrpheusVoice::Mia => "mia",
        OrpheusVoice::Zac => "zac",
        OrpheusVoice::Zoe => "zoe",
    }
}

pub fn models_dir(global_config_dir: &Path) -> PathBuf {
    global_config_dir.join("models").join("orpheus")
}

async fn ensure_orpheus_gguf(global_config_dir: &Path, model: OrpheusModel) -> Result<PathBuf> {
    let dir = models_dir(global_config_dir);
    let path = dir.join(orpheus_filename(model));
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    info!(model = ?model, url = orpheus_url(model), "downloading orpheus gguf");
    crate::downloads::download_to(orpheus_url(model), &path).await?;
    info!(model = ?model, path = %path.display(), "orpheus gguf ready");
    Ok(path)
}

async fn ensure_snac_onnx(global_config_dir: &Path) -> Result<PathBuf> {
    let dir = models_dir(global_config_dir);
    let path = dir.join(SNAC_FILENAME);
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    info!(url = SNAC_URL, "downloading snac decoder onnx");
    crate::downloads::download_to(SNAC_URL, &path).await?;
    info!(path = %path.display(), "snac decoder ready");
    Ok(path)
}

/// Registry of loaded backends. Cloned into every connection's
/// dispatch path; the `Mutex` is held only for the short
/// lookup/insert windows — the long-running download + factory load
/// runs without the lock held.
#[derive(Clone)]
pub struct TtsBackends {
    inner: Arc<Inner>,
}

struct Inner {
    services: Mutex<HashMap<BackendKey, Arc<TtsService>>>,
    sink: mpsc::UnboundedSender<TtsEvent>,
    config_dir: PathBuf,
}

impl TtsBackends {
    pub fn new(config_dir: PathBuf, sink: mpsc::UnboundedSender<TtsEvent>) -> Self {
        Self {
            inner: Arc::new(Inner {
                services: Mutex::new(HashMap::new()),
                sink,
                config_dir,
            }),
        }
    }

    pub fn lookup(&self, backend: &TtsBackend) -> Option<Arc<TtsService>> {
        let key = BackendKey::from_backend(backend);
        let guard = self.inner.services.lock().expect("tts backends poisoned");
        guard.get(&key).cloned()
    }

    /// Download (if needed) and load the backend so a subsequent
    /// `OpenTtsStream` for the same key resolves in
    /// [`Self::lookup`]. Idempotent: a second `ensure` for an
    /// already-loaded key returns immediately without touching disk.
    pub async fn ensure(&self, backend: TtsBackend) -> Result<()> {
        let key = BackendKey::from_backend(&backend);
        if self
            .inner
            .services
            .lock()
            .expect("tts backends poisoned")
            .contains_key(&key)
        {
            return Ok(());
        }
        let TtsBackend::Orpheus { model, .. } = backend;
        let gguf = ensure_orpheus_gguf(&self.inner.config_dir, model).await?;
        let snac = ensure_snac_onnx(&self.inner.config_dir).await?;
        let sink = self.inner.sink.clone();
        let service = tokio::task::spawn_blocking(move || -> Result<Arc<TtsService>> {
            let factory = OrpheusFactory::load(&gguf, &snac)
                .map_err(|e| anyhow!("load orpheus factory: {e}"))?;
            let service = TtsService::new(Box::new(factory), sink, DEFAULT_WORKER_COUNT)
                .map_err(|e| anyhow!("spawn tts service: {e}"))?;
            Ok(Arc::new(service))
        })
        .await
        .map_err(|e| anyhow!("tts backend load task panicked: {e}"))??;
        let mut guard = self.inner.services.lock().expect("tts backends poisoned");
        // Race: another `ensure` for the same key may have completed
        // while we were loading. Keep the first installed service so
        // already-opened streams stay valid.
        guard.entry(key).or_insert(service);
        Ok(())
    }
}
