//! TTS model fetch + backend registry, CP-side.
//!
//! `TtsBackends` is a process-wide cache of loaded
//! [`lutin_tts::TtsService`]s, keyed by the fields that determine
//! which weights are in VRAM. Per-utterance config (voice, speed) is
//! *not* part of the key — two streams with different voices share
//! one loaded model.
//!
//! `EnsureTtsBackend` is the only entry point that triggers a download
//! / factory load; `OpenTtsStream` is fast-path lookup that returns
//! `TtsBackendNotReady` on miss. The split keeps the open path
//! predictable and gives the UI a single place to surface progress.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use lutin_control_protocol::{Event, OrpheusModel, OrpheusVoice, TtsBackend};
use lutin_tts::{DEFAULT_WORKER_COUNT, OrpheusFactory, TtsEvent, TtsService};
use tokio::fs;
use tokio::sync::{Mutex as TokioMutex, broadcast, mpsc};
use tracing::{info, warn};

/// Identity that determines weights-on-GPU. Two `TtsBackend`s with
/// the same identity share a `TtsService`. Voice / speed are excluded
/// — they ride along with each `speak` call, not with the loaded
/// model. Today this is just the Orpheus model variant; widen when a
/// second backend lands.
type BackendIdentity = OrpheusModel;

fn identity(b: &TtsBackend) -> BackendIdentity {
    match b {
        TtsBackend::Orpheus { model, .. } => *model,
    }
}

// Repo lives at the upper-case URL but the actual GGUF file inside
// the repo is lower-case (`q4_k_m.gguf`). The legacy engine pointed
// at the upper-case filename which now 404s. Keep the on-disk cache
// filename matching the upstream filename so a future repo move only
// changes one place.
fn orpheus_filename(m: OrpheusModel) -> &'static str {
    match m {
        OrpheusModel::ThreeBQ4KM => "orpheus-3b-0.1-ft-q4_k_m.gguf",
    }
}

fn orpheus_url(m: OrpheusModel) -> &'static str {
    match m {
        OrpheusModel::ThreeBQ4KM => {
            "https://huggingface.co/isaiahbjork/orpheus-3b-0.1-ft-Q4_K_M-GGUF/resolve/main/orpheus-3b-0.1-ft-q4_k_m.gguf"
        }
    }
}

const SNAC_FILENAME: &str = "snac_decoder_model.onnx";
const SNAC_URL: &str =
    "https://huggingface.co/onnx-community/snac_24khz-ONNX/resolve/main/onnx/decoder_model.onnx";

/// Wire id → worker id. Widening, loss-free.
pub fn to_worker_id(id: lutin_control_protocol::TtsStreamId) -> lutin_tts::StreamId {
    lutin_tts::StreamId(id.0 as u64)
}

/// Worker id → wire id. CP only ever pushes wire-allocated u32s into
/// the worker, so the narrow back is loss-free; if a worker ever
/// produces an id outside u32 range we drop it on the floor (the
/// alternative — silent truncation — would deliver audio for the
/// wrong stream).
pub fn from_worker_id(id: lutin_tts::StreamId) -> Option<lutin_control_protocol::TtsStreamId> {
    u32::try_from(id.0).ok().map(lutin_control_protocol::TtsStreamId)
}

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

fn models_dir(global_config_dir: &Path) -> PathBuf {
    global_config_dir.join("models").join("orpheus")
}

/// Broadcast `TtsBackendDownload` progress for `file` against
/// `backend`. `send` errors are ignored — they only happen when no
/// subscribers exist (the broadcast is fan-out, not request/reply),
/// and the download itself shouldn't fail just because no one's
/// listening.
fn emit_progress(
    events: &broadcast::Sender<Event>,
    backend: &TtsBackend,
    file: &str,
    downloaded: u64,
    total: Option<u64>,
) {
    let _ = events.send(Event::TtsBackendDownload {
        backend: backend.clone(),
        file: file.to_owned(),
        downloaded,
        total,
    });
}

async fn ensure_orpheus_gguf(
    global_config_dir: &Path,
    model: OrpheusModel,
    backend: &TtsBackend,
    events: &broadcast::Sender<Event>,
) -> Result<PathBuf> {
    let dir = models_dir(global_config_dir);
    let filename = orpheus_filename(model);
    let path = dir.join(filename);
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    info!(model = ?model, url = orpheus_url(model), "downloading orpheus gguf");
    crate::downloads::download_to_with_progress(orpheus_url(model), &path, |downloaded, total| {
        emit_progress(events, backend, filename, downloaded, total);
    })
    .await?;
    info!(model = ?model, path = %path.display(), "orpheus gguf ready");
    Ok(path)
}

async fn ensure_snac_onnx(
    global_config_dir: &Path,
    backend: &TtsBackend,
    events: &broadcast::Sender<Event>,
) -> Result<PathBuf> {
    let dir = models_dir(global_config_dir);
    let path = dir.join(SNAC_FILENAME);
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    info!(url = SNAC_URL, "downloading snac decoder onnx");
    crate::downloads::download_to_with_progress(SNAC_URL, &path, |downloaded, total| {
        emit_progress(events, backend, SNAC_FILENAME, downloaded, total);
    })
    .await?;
    info!(path = %path.display(), "snac decoder ready");
    Ok(path)
}

/// Registry of loaded backends. Cloned into every connection's
/// dispatch path; the `Mutex` is held only for the short
/// lookup/insert windows — the long-running download + factory load
/// runs without the lock held.
///
/// Storage is a `Vec<(BackendIdentity, Arc<TtsService>)>` rather than
/// a `HashMap`: realistic N is 1 (today) or a small handful (once
/// Kokoro / cloud backends land), so the hashing overhead and extra
/// allocation pay off nowhere — a linear scan over &lt; 8 entries beats
/// it every time.
#[derive(Clone)]
pub struct TtsBackends {
    inner: Arc<Inner>,
}

struct Inner {
    services: Mutex<Vec<(BackendIdentity, Arc<TtsService>)>>,
    /// Per-identity gate held across the slow path of `ensure` — the
    /// download + factory load. Two concurrent `EnsureTtsBackend`
    /// requests for the same identity (e.g. from React StrictMode's
    /// double-mounted effect, or a user toggle bouncing) would
    /// otherwise both call `download_to_with_progress` against the
    /// same `<dest>.tmp` file and trample each other so neither
    /// finishes. With this gate, the second call awaits the first
    /// and then hits `lookup`'s fast path.
    ensure_gates: Mutex<Vec<(BackendIdentity, Arc<TokioMutex<()>>)>>,
    sink: mpsc::UnboundedSender<TtsEvent>,
    config_dir: PathBuf,
}

impl TtsBackends {
    pub fn new(config_dir: PathBuf, sink: mpsc::UnboundedSender<TtsEvent>) -> Self {
        Self {
            inner: Arc::new(Inner {
                services: Mutex::new(Vec::new()),
                ensure_gates: Mutex::new(Vec::new()),
                sink,
                config_dir,
            }),
        }
    }

    fn ensure_gate(&self, id: BackendIdentity) -> Arc<TokioMutex<()>> {
        let mut guard = self.inner.ensure_gates.lock().expect("ensure_gates poisoned");
        if let Some((_, m)) = guard.iter().find(|(k, _)| *k == id) {
            return m.clone();
        }
        let m = Arc::new(TokioMutex::new(()));
        guard.push((id, m.clone()));
        m
    }

    pub fn lookup(&self, backend: &TtsBackend) -> Option<Arc<TtsService>> {
        let id = identity(backend);
        let guard = self.inner.services.lock().expect("tts backends poisoned");
        guard
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, s)| s.clone())
    }

    /// Download (if needed) and load the backend so a subsequent
    /// `OpenTtsStream` for the same identity resolves in
    /// [`Self::lookup`]. Idempotent: a second `ensure` for an
    /// already-loaded identity returns immediately without touching
    /// disk. `events` is used to broadcast `TtsBackendDownload`
    /// progress while weights are being fetched; a no-op channel is
    /// fine (`send` errors are ignored).
    pub async fn ensure(
        &self,
        backend: &TtsBackend,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        let id = identity(backend);
        if self.lookup(backend).is_some() {
            return Ok(());
        }
        // Serialize the slow path per identity. A second caller waits
        // here until the first finishes, then re-checks `lookup` —
        // the winner installed the service, so the loser is a no-op.
        let gate = self.ensure_gate(id);
        let _gate_guard = gate.lock().await;
        if self.lookup(backend).is_some() {
            return Ok(());
        }
        let TtsBackend::Orpheus { model, .. } = backend;
        let gguf = ensure_orpheus_gguf(&self.inner.config_dir, *model, backend, events).await?;
        let snac = ensure_snac_onnx(&self.inner.config_dir, backend, events).await?;
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
        // Race: another `ensure` for the same identity may have
        // completed while we were loading. Keep the first installed
        // service so already-opened streams stay valid; the loser's
        // service drops here, tearing down its worker pool. A warn
        // makes the duplicate work visible in logs without changing
        // behaviour.
        if guard.iter().any(|(k, _)| *k == id) {
            warn!(
                identity = ?id,
                "concurrent ensure raced; discarding duplicate-loaded service",
            );
        } else {
            guard.push((id, service));
        }
        Ok(())
    }
}
