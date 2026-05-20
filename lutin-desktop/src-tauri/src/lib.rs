//! Tauri chrome entry point: owns the CP client, wires Tauri commands
//! that JS calls, and pumps `CpUpdate` events out as Tauri events the
//! React chrome listens to.

mod audio;
mod bridge;
mod bundles;
mod capability;
mod cp;
mod dispatch;
mod keybind;
#[cfg(target_os = "linux")]
mod keybind_portal;
mod overlay;
mod plugin_protocol;
mod settings;
mod shim_protocol;
mod tts_dispatch;
mod tts_playback;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};

use lutin_control_protocol::{
    Event as CpEvent, Request, Response, ResponseOk, SessionId, Slug, WorkflowId,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;
use tracing_subscriber::EnvFilter;

use crate::audio::Capture;
use crate::tts_playback::TtsPlayback;
use crate::bridge::{BridgeCmd, BridgeHandle, EngineBytes};
use crate::bundles::BundleCache;
use crate::cp::{CpClient, CpCommand, CpConfig, CpUpdate, RequestId, Token};

use crate::settings::{ConnectionProfile, DesktopSettings};

/// Which backend is delivering global hotkey events. Picked at
/// startup based on session type and a successful portal handshake;
/// thereafter immutable. Each variant *owns* its backend-specific
/// state — the registry only exists when we actually use it (no
/// "Plugin variant + dangling registry field" invariant to maintain).
pub enum KeybindBackend {
    /// `tauri-plugin-global-shortcut` path (X11, macOS, Windows, and
    /// Wayland fallback when the portal is unavailable). Owns the
    /// in-process registry the plugin handler reads on each event.
    Plugin(crate::keybind::KeybindRegistry),
    /// XDG GlobalShortcuts portal (Wayland). Background task owns
    /// the D-Bus session and dispatches Activated/Deactivated.
    #[cfg(target_os = "linux")]
    Portal(crate::keybind_portal::PortalBackend),
}

/// What `keybind_backend` returns to JS. Externally tagged on `kind`
/// — the portal variant carries the id prefix and snippet template
/// so the settings UI doesn't re-derive the format and stays in sync
/// when the wire format changes.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeybindBackendInfo {
    Plugin,
    Portal {
        /// Per-bind shortcut id is `format!("{id_prefix}{idx}")`.
        id_prefix: String,
        /// Compositor snippet template, with literal `{combo}` and
        /// `{id}` placeholders the JS substitutes. Today this emits
        /// a Hyprland line; a future Sway/etc. branch picks a
        /// different template at construction time.
        snippet_template: String,
    },
}

/// Snapshot of the connection's last known state. Mirrors the React
/// `ConnState` type 1:1 — Tauri serializes externally-tagged on `kind`
/// so JS can discriminate without unwrapping a wrapper.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConnSnapshot {
    NoConfig,
    Connecting,
    Connected,
    Disconnected,
    Rejected { reason: String },
    Error { error: String },
}

/// Tauri-managed state. All command handlers reach into this. Long
/// reads/writes must not block — the inner mutexes are held only for
/// the duration of a HashMap mutation or a `CpClient::send` call.
struct AppState {
    tokio: tokio::runtime::Handle,
    cp: Mutex<CpClient>,
    /// Stable updates sink, cloned into every worker so the drainer
    /// task can outlive reconnects.
    evt_tx: mpsc::UnboundedSender<CpUpdate>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Response>>>,
    next_request_id: AtomicU64,
    settings: Mutex<DesktopSettings>,
    /// Workflow plugin bundles unpacked under the app cache dir.
    /// Read by the `lutin-plugin` URI scheme handler.
    pub bundles: BundleCache,
    /// Live engine bridges keyed by session id. Open via
    /// `workflow_session_open`; closed when the JS side calls
    /// `workflow_session_close` or chrome shuts down.
    bridges: Mutex<HashMap<String, BridgeHandle>>,
    /// Last known connection state, updated by the drainer task.
    /// Read by `cp_status` so JS can initialize without racing
    /// against a `cp:connected` event that fires before the
    /// listener attaches.
    conn: Mutex<ConnSnapshot>,
    /// Backend delivering hotkey events. Set exactly once during
    /// `setup()` — `setup` builds the appropriate variant (portal
    /// proxy on Wayland, plugin registry elsewhere) and seals it.
    /// All readers see the same value for the rest of the app's
    /// life, no locking, no clones, no poison cases. The plugin
    /// path's `KeybindRegistry` lives *inside* the variant, so the
    /// "registry only matters when Plugin is selected" invariant is
    /// compiler-enforced.
    pub keybind_backend: OnceLock<KeybindBackend>,
    /// Microphone capture, owned for the app's lifetime so the cpal
    /// stream-control thread is built once. `None` if no input device
    /// is available — hotkey actions log and no-op in that case rather
    /// than panicking.
    pub audio: AudioHandle,
    /// In-flight PTT bookkeeping. `Some` between `ptt_down` and
    /// `ptt_up`; lives on `AppState` (not a module-level static) so
    /// it's testable, scoped to the app's lifetime, and cleanly torn
    /// down with the rest of the state. `Mutex<Option<…>>` rather
    /// than an actor task because the lock is only ever held for a
    /// single replace/take (no `.await` spans), and the actor pattern
    /// would multiply the channel/task plumbing for what's
    /// fundamentally a one-slot register.
    pub active_ptt: std::sync::Mutex<Option<dispatch::ActivePtt>>,
    /// What the chrome currently considers "active" — populated by
    /// `set_active_session` from `<PluginIframe>`'s mount/unmount
    /// hooks. Dispatch reads this to resolve `Target::ActiveWorkflow`
    /// and to gate `Target::Workflow {…}` against the running session
    /// for that workflow id. `None` while no plugin iframe is mounted
    /// (e.g. on the Settings tab).
    pub active_session: Mutex<Option<ActiveSession>>,
    /// TTS playback. Mirrors `audio` in lifecycle — built once at
    /// startup and held for the app's lifetime so the cpal output
    /// stream's control thread is constructed exactly once. `None` if
    /// no output device is available; TTS commands then surface a
    /// device-unavailable error rather than panicking.
    pub tts_playback: TtsPlaybackHandle,
}

/// What the React side reports about the iframe currently in front.
/// Carries the manifest's `capabilities` set so dispatch can do the
/// `receive_transcription` gate without round-tripping back through
/// the bundle cache. Cheap copy on the routing path.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct ActiveSession {
    pub session: SessionId,
    pub workflow: WorkflowId,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Wrapper around `Option<Capture>` so `dispatch.rs` can unconditionally
/// call `audio.start()` / `audio.stop()` whether or not a mic is
/// available. Keeps the call sites readable; the no-mic case logs once
/// at startup and `start()` returns `None` so dispatch falls through.
pub struct AudioHandle(Option<Capture>);

impl AudioHandle {
    pub fn new(initial_device: Option<String>) -> Self {
        match Capture::new(initial_device) {
            Ok(c) => Self(Some(c)),
            Err(e) => {
                warn!(error = %e, "audio capture unavailable; hotkey PTT will no-op");
                Self(None)
            }
        }
    }

    /// Forward a device swap to the underlying `Capture`. No-op when
    /// the constructor failed (no mic available).
    pub fn set_device(&self, device: Option<String>) {
        if let Some(c) = &self.0 {
            c.set_device(device);
        }
    }

    /// Arm capture and return the chunk receiver, or `None` when no
    /// mic is available. Caller drives the receiver to completion (or
    /// drops it) and pairs it with `stop()`.
    pub fn start(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<lutin_control_protocol::MonoPcm16k>> {
        self.0.as_ref().map(|c| c.start())
    }

    pub fn stop(&self) {
        if let Some(c) = &self.0 {
            c.stop();
        }
    }
}

/// Mirror of `AudioHandle` for the playback side: keeps every call
/// site infallible and makes the no-output-device case a logged
/// no-op instead of a panic. CP-side audio chunks for streams whose
/// playback didn't initialise are silently dropped on enqueue.
pub struct TtsPlaybackHandle(Option<TtsPlayback>);

impl TtsPlaybackHandle {
    pub fn new(initial_device: Option<String>) -> Self {
        match TtsPlayback::new(initial_device) {
            Ok(p) => Self(Some(p)),
            Err(e) => {
                warn!(error = %e, "tts playback unavailable; audio chunks will be dropped");
                Self(None)
            }
        }
    }

    pub fn set_device(&self, device: Option<String>) {
        if let Some(p) = &self.0 {
            p.set_device(device);
        }
    }

    pub fn register(&self, stream_id: lutin_control_protocol::TtsStreamId, session: SessionId) {
        if let Some(p) = &self.0 {
            p.register(stream_id, session);
        }
    }

    pub fn enqueue(&self, stream_id: lutin_control_protocol::TtsStreamId, chunk: &[u8]) {
        if let Some(p) = &self.0 {
            p.enqueue(stream_id, chunk);
        }
    }

    pub fn cancel(&self, stream_id: lutin_control_protocol::TtsStreamId) {
        if let Some(p) = &self.0 {
            p.cancel(stream_id);
        }
    }

    pub fn set_speed(&self, stream_id: lutin_control_protocol::TtsStreamId, speed: f32) {
        if let Some(p) = &self.0 {
            p.set_speed(stream_id, speed);
        }
    }

    pub fn unregister(&self, stream_id: lutin_control_protocol::TtsStreamId) {
        if let Some(p) = &self.0 {
            p.unregister(stream_id);
        }
    }

    pub fn set_active_session(&self, active: Option<&SessionId>) {
        if let Some(p) = &self.0 {
            p.set_active_session(active);
        }
    }
}

impl AppState {
    fn alloc_request_id(&self) -> RequestId {
        RequestId(self.next_request_id.fetch_add(1, Ordering::Relaxed))
    }
}

/// Convert a `ConnectionProfile` into a usable `CpConfig`. Returns
/// `Ok(None)` when the profile is incomplete (e.g. blank token) so the
/// chrome stays in "no active connection" mode without erroring.
fn profile_to_config(profile: &ConnectionProfile) -> Result<Option<CpConfig>, String> {
    if profile.addr.trim().is_empty() || profile.token.trim().is_empty() {
        return Ok(None);
    }
    let url = url::Url::parse(&format!("ws://{}", profile.addr))
        .map_err(|e| format!("invalid addr {:?}: {e}", profile.addr))?;
    let token = Token::new(profile.token.clone()).map_err(|e| e.to_string())?;
    Ok(Some(CpConfig { url, token }))
}

/// Why a `cp_dispatch` couldn't deliver a `Response`. Distinct from
/// `Response::Err(ApiError)` — that's CP returning a typed
/// application error; this enum is *transport* failure (CP wasn't
/// connected, or the pending entry was dropped before the reply
/// arrived). Two-variant: there's nothing else that can go wrong at
/// this layer, and stringly-typed transport errors made dispatch
/// site code show opaque messages on the listening overlay.
#[derive(Debug, Clone)]
pub enum TransportError {
    NotConnected,
    Cancelled,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => f.write_str("control panel not connected"),
            Self::Cancelled => f.write_str("request cancelled"),
        }
    }
}

/// Send `request` to CP and await its `Response`. Lower-level helper
/// used by both the JS-facing `cp_send` command and Rust commands
/// (e.g. `workflow_session_open`) that need to call CP without
/// round-tripping through JS.
pub(crate) async fn cp_dispatch(
    state: &AppState,
    request: Request,
) -> Result<Response, TransportError> {
    let id = state.alloc_request_id();
    let (tx, rx) = oneshot::channel();
    state
        .pending
        .lock()
        .expect("pending mutex poisoned")
        .insert(id.0, tx);
    let send_res = state.cp.lock().expect("cp mutex poisoned").send(CpCommand::Send {
        request_id: id,
        request,
    });
    if send_res.is_err() {
        state
            .pending
            .lock()
            .expect("pending mutex poisoned")
            .remove(&id.0);
        return Err(TransportError::NotConnected);
    }
    rx.await.map_err(|_| TransportError::Cancelled)
}

#[tauri::command]
async fn cp_send(state: State<'_, AppState>, request: Request) -> Result<Response, String> {
    // The Tauri command boundary is the one place where we collapse
    // the typed transport error back to a string — JS callers just
    // treat the failure as opaque. Internal Rust callers consume
    // `cp_dispatch` directly and pattern-match on the typed enum.
    cp_dispatch(&state, request).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn cp_status(state: State<'_, AppState>) -> ConnSnapshot {
    state.conn.lock().expect("conn mutex poisoned").clone()
}

/// Reply to `workflow_open_plugin`. JS sets the iframe `src` to `url`
/// and uses `manifest` to decide which capabilities to wire into the
/// plugin's `window.lutin` shim once the MessagePort handshake lands.
#[derive(Clone, Debug, Serialize)]
pub struct PluginOpened {
    pub url: String,
    pub manifest: PluginManifest,
}

/// Mirrors the plugin's `lutin.workflow.json` (subset). Fields beyond
/// what chrome cares about are ignored at parse time.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct PluginManifest {
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// What this workflow can be a *target of* (vs `permissions`,
    /// which is what it can *do*). Slice 3 introduces
    /// `"receive_transcription"`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub icon: String,
}

fn default_entry() -> String {
    "index.html".to_owned()
}

/// Ensure a workflow's plugin bundle is unpacked locally and return
/// the iframe URL + parsed manifest. The desktop fetches bundles from
/// CP on first use (and on digest mismatch) and caches them under the
/// Tauri app-cache dir; subsequent calls hit the cache.
#[tauri::command]
async fn workflow_open_plugin(
    state: State<'_, AppState>,
    workflow: WorkflowId,
    digest: String,
) -> Result<PluginOpened, String> {
    let dir = match state.bundles.lookup(&workflow, &digest) {
        Some(p) => p,
        None => {
            let resp = cp_dispatch(
                &state,
                Request::GetWorkflowBundle { id: workflow.clone() },
            )
            .await
            .map_err(|e| e.to_string())?;
            let bytes = match resp {
                Response::Ok(ResponseOk::WorkflowBundle { digest: got, bytes, .. }) => {
                    if got != digest {
                        warn!(
                            workflow = %workflow.as_str(),
                            expected = %digest,
                            actual = %got,
                            "bundle digest mismatch — using fetched digest"
                        );
                    }
                    bytes
                }
                Response::Ok(other) => return Err(format!("unexpected response: {other:?}")),
                Response::Err(e) => return Err(format!("CP error: {e}")),
            };
            state
                .bundles
                .install(&workflow, &digest, &bytes)
                .map_err(|e| format!("install bundle: {e}"))?
        }
    };

    let manifest_path = dir.join("lutin.workflow.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("read manifest: {e}"))?;
    let manifest: PluginManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("parse manifest: {e}"))?;

    let url = plugin_protocol::url_for(&workflow, &manifest.entry);
    Ok(PluginOpened { url, manifest })
}

/// Open the engine WebSocket for a session and stash the bridge in
/// AppState keyed by session id. Idempotent — if a bridge is already
/// open for this session id, the existing one is reused. Token never
/// crosses the JS boundary; chrome holds it for the lifetime of the
/// bridge.
#[tauri::command]
async fn workflow_session_open(
    state: State<'_, AppState>,
    slug: Slug,
    session: SessionId,
) -> Result<(), String> {
    let key = session.as_str().to_owned();
    if state.bridges.lock().expect("bridges mutex poisoned").contains_key(&key) {
        return Ok(());
    }

    // Try OpenSession first (cheap; just re-mints a token if running).
    // If CP reports the session as not-running, transparently fall
    // back to ResumeSession so the chrome doesn't need to know
    // whether the engine container is up — clicking a list row Just
    // Works.
    let open_resp = cp_dispatch(
        &state,
        Request::OpenSession { slug: slug.clone(), session: session.clone() },
    )
    .await
    .map_err(|e| e.to_string())?;
    let endpoint = match open_resp {
        Response::Ok(ResponseOk::SessionOpened(ep)) => ep,
        Response::Ok(other) => return Err(format!("unexpected response: {other:?}")),
        Response::Err(_) => {
            // Most likely SessionNotFound (registry doesn't have it
            // because container is dormant). Resume covers other CP
            // errors too — if it also fails, the second error is the
            // useful one to surface.
            let resume_resp = cp_dispatch(
                &state,
                Request::ResumeSession { slug, session: session.clone() },
            )
            .await
            .map_err(|e| e.to_string())?;
            match resume_resp {
                Response::Ok(ResponseOk::SessionResumed { endpoint, .. }) => endpoint,
                Response::Ok(other) => {
                    return Err(format!("unexpected resume response: {other:?}"));
                }
                Response::Err(e) => return Err(format!("CP resume error: {e}")),
            }
        }
    };

    let url = format!("ws://{}", endpoint.addr);
    let handle = bridge::connect(&state.tokio, url, endpoint.token).await?;
    state
        .bridges
        .lock()
        .expect("bridges mutex poisoned")
        .insert(key, handle);
    Ok(())
}

/// Forward a request body to the engine. Resolves with the body of
/// the matching `Frame::Payload` reply.
#[tauri::command]
async fn workflow_session_request(
    state: State<'_, AppState>,
    session: SessionId,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let handle = state
        .bridges
        .lock()
        .expect("bridges mutex poisoned")
        .get(session.as_str())
        .cloned()
        .ok_or_else(|| format!("no bridge for session {}", session.as_str()))?;
    let (tx, rx) = oneshot::channel();
    handle
        .send(BridgeCmd::Request { body, reply: tx })
        .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| "bridge dropped reply".to_string())?
}

/// Subscribe to engine broadcasts for `session`. Each `Frame::Broadcast`
/// body is delivered on `channel`. Channel closes when the bridge
/// teardown happens (WS closed, or session closed via
/// `workflow_session_close`).
#[tauri::command]
fn workflow_session_subscribe(
    state: State<'_, AppState>,
    session: SessionId,
    channel: tauri::ipc::Channel<EngineBytes>,
) -> Result<(), String> {
    let handle = state
        .bridges
        .lock()
        .expect("bridges mutex poisoned")
        .get(session.as_str())
        .cloned()
        .ok_or_else(|| format!("no bridge for session {}", session.as_str()))?;
    handle
        .send(BridgeCmd::Subscribe { channel })
        .map_err(|e| e.to_string())
}

/// Tear down a session bridge. Safe to call on a session id that has
/// no bridge (no-op).
#[tauri::command]
fn workflow_session_close(state: State<'_, AppState>, session: SessionId) {
    if let Some(handle) = state
        .bridges
        .lock()
        .expect("bridges mutex poisoned")
        .remove(session.as_str())
    {
        let _ = handle.send(BridgeCmd::Close);
    }
}

/// Which keybind backend is active: `"plugin"` or `"portal"`. JS
/// uses this to decide whether the Keybinds settings UI should
/// surface portal ids (with copy-pastable compositor snippets) or
/// just the combo strings.
/// Build the keybind backend at startup. On Wayland we try the
/// portal first; on failure (no `xdg-desktop-portal-*` running, user
/// denied the bind dialog, etc.) we fall back to the plugin path.
/// The fallback is honest about the limitation — on Wayland the
/// plugin's X11 grabs only fire while focus is on an XWayland
/// window — but it's better than silently dead hotkeys, and the
/// settings UI surfaces the actual backend so the user knows.
fn build_keybind_backend(
    app: &AppHandle,
    prefer_portal: bool,
    initial: &[settings::KeyBind],
) -> KeybindBackend {
    #[cfg(target_os = "linux")]
    if prefer_portal {
        match keybind_portal::PortalBackend::start(app.clone(), initial.to_vec()) {
            Ok(p) => {
                tracing::info!("keybind backend: portal (Wayland)");
                return KeybindBackend::Portal(p);
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "portal start failed; falling back to plugin backend (XWayland-only on Wayland)",
                );
            }
        }
    }
    let registry = keybind::KeybindRegistry::default();
    let parsed = keybind::parse_lossy(initial);
    let failed = keybind::reconcile(app, &registry, parsed);
    if !failed.is_empty() {
        warn!(?failed, "some keybinds failed to register");
    }
    tracing::info!("keybind backend: plugin");
    KeybindBackend::Plugin(registry)
}

/// What `settings_set` is going to apply once the file is saved.
/// Built up-front so any parse error short-circuits before we touch
/// disk or OS registration.
enum ReconcilePlan {
    Plugin(Vec<(tauri_plugin_global_shortcut::Shortcut, settings::KeyBind)>),
    #[cfg(target_os = "linux")]
    Portal(Vec<settings::KeyBind>),
}

/// Hyprland snippet template. Single source of truth for the bind
/// line format. JS substitutes `{combo}` and `{id}`; nothing else
/// re-derives this string.
///
/// The namespace before the colon is empty (`global, :{id}`) rather
/// than `lutin:{id}` because xdg-desktop-portal-hyprland scopes
/// shortcut ids by the registering D-Bus connection's app id, and we
/// don't currently set one — matching `:` (any-app) is what the
/// portal accepts in practice. If we ever ship a desktop file with a
/// proper reverse-DNS app id, this template gets updated to use it.
const HYPRLAND_SNIPPET_TEMPLATE: &str = "bind = , {combo}, global, :{id}";

#[tauri::command]
fn keybind_backend(state: State<'_, AppState>) -> KeybindBackendInfo {
    match state.keybind_backend.get() {
        Some(KeybindBackend::Plugin(_)) | None => KeybindBackendInfo::Plugin,
        #[cfg(target_os = "linux")]
        Some(KeybindBackend::Portal(_)) => KeybindBackendInfo::Portal {
            id_prefix: keybind_portal::PORTAL_ID_PREFIX.to_owned(),
            snippet_template: HYPRLAND_SNIPPET_TEMPLATE.to_owned(),
        },
    }
}

/// Report which session iframe is currently in front. Called by
/// `<PluginIframe>` on mount (`Some`) and unmount (`None`). Drives
/// `Target::ActiveWorkflow` dispatch and feeds the capability gate for
/// per-session transcription delivery.
#[tauri::command]
fn set_active_session(state: State<'_, AppState>, active: Option<ActiveSession>) {
    // Mirror the change to the playback module so it can drop queued
    // audio for streams bound to the previously-active session — held
    // audio after a context switch is worse than losing it.
    state
        .tts_playback
        .set_active_session(active.as_ref().map(|a| &a.session));
    *state.active_session.lock().expect("active_session poisoned") = active;
}

#[tauri::command]
fn audio_input_devices() -> Vec<String> {
    audio::list_input_devices()
}

#[tauri::command]
fn audio_output_devices() -> Vec<String> {
    tts_playback::list_output_devices()
}

#[tauri::command]
fn settings_get(state: State<'_, AppState>) -> DesktopSettings {
    state.settings.lock().expect("settings mutex poisoned").clone()
}

#[tauri::command]
async fn settings_set(
    app: AppHandle,
    state: State<'_, AppState>,
    new: DesktopSettings,
) -> Result<(), String> {
    // Strict-parse the combos up front for the plugin backend so a
    // bad entry is rejected before we save the file or touch OS
    // registration. On portal the combos are descriptive hints sent
    // to the compositor — parse failure means "the suggestion isn't
    // a real key" and that's not a save-blocking error. Branching
    // here also forces both arms to materialise the work they need
    // before `new.save()`, so a parse failure doesn't leave a
    // partially-written settings file behind.
    let effective = new.effective_keybinds();
    let backend = state
        .keybind_backend
        .get()
        .expect("keybind backend not initialised");
    let plan = match backend {
        KeybindBackend::Plugin(_) => {
            let parsed = keybind::parse(&effective).map_err(|e| {
                format!("invalid keybind combo {:?}: {}", e.combo, e.source)
            })?;
            ReconcilePlan::Plugin(parsed)
        }
        #[cfg(target_os = "linux")]
        KeybindBackend::Portal(_) => ReconcilePlan::Portal(effective),
    };
    new.save()?;
    let cfg = match new.active() {
        Some(p) => profile_to_config(p)?,
        None => None,
    };
    // Apply audio device pinning if it changed. Comparing against the
    // currently-loaded settings (under the lock) avoids re-issuing a
    // SetDevice — and the queue clear it carries — when the user
    // saves an unrelated tab.
    let prev_audio = {
        let g = state.settings.lock().expect("settings mutex poisoned");
        g.audio.clone()
    };
    if prev_audio.input != new.audio.input {
        state.audio.set_device(new.audio.input.clone());
    }
    if prev_audio.output != new.audio.output {
        state.tts_playback.set_device(new.audio.output.clone());
    }
    *state.settings.lock().expect("settings mutex poisoned") = new;
    match (plan, backend) {
        (ReconcilePlan::Plugin(parsed), KeybindBackend::Plugin(reg)) => {
            let failed = keybind::reconcile(&app, reg, parsed);
            if !failed.is_empty() {
                warn!(?failed, "some keybinds failed to register on settings_set");
            }
        }
        #[cfg(target_os = "linux")]
        (ReconcilePlan::Portal(binds), KeybindBackend::Portal(p)) => {
            if let Err(e) = p.reconcile(binds).await {
                warn!(error = %e, "portal reconcile failed on settings_set");
            }
        }
        // Plan and backend are derived from the same `backend` ref
        // in the same function, so a mismatch can't happen at
        // runtime. Fail loud rather than silently if it ever does.
        _ => unreachable!("plan/backend mismatch"),
    }
    // Drop pending requests — they were scoped to the previous CP and
    // their senders would leak as never-fulfilled.
    state
        .pending
        .lock()
        .expect("pending mutex poisoned")
        .clear();
    let initial = match cfg {
        Some(_) => ConnSnapshot::Connecting,
        None => ConnSnapshot::NoConfig,
    };
    *state.conn.lock().expect("conn mutex poisoned") = initial;
    state
        .cp
        .lock()
        .expect("cp mutex poisoned")
        .reconnect(&state.tokio, cfg, state.evt_tx.clone());
    Ok(())
}

/// Drain `evt_rx` and either resolve a pending request (for
/// `Response`) or fan out as a Tauri event (for everything else).
/// Also keeps `AppState.conn` in sync so `cp_status` can answer
/// without racing against the event listener attaching.
async fn drain_updates(
    app: AppHandle,
    mut evt_rx: mpsc::UnboundedReceiver<CpUpdate>,
) {
    while let Some(update) = evt_rx.recv().await {
        let state = app.state::<AppState>();
        match update {
            CpUpdate::Response { request_id, response } => {
                let tx = state
                    .pending
                    .lock()
                    .expect("pending mutex poisoned")
                    .remove(&request_id.0);
                if let Some(tx) = tx {
                    let _ = tx.send(response);
                } else {
                    warn!(?request_id, "response for unknown request id");
                }
            }
            CpUpdate::Connected => {
                set_conn(&state, ConnSnapshot::Connected);
                emit(&app, "cp:connected", ());
            }
            CpUpdate::Disconnected => {
                set_conn(&state, ConnSnapshot::Disconnected);
                emit(&app, "cp:disconnected", ());
            }
            CpUpdate::HandshakeRejected(reason) => {
                set_conn(&state, ConnSnapshot::Rejected { reason: reason.clone() });
                emit(&app, "cp:handshake-rejected", reason);
            }
            CpUpdate::ConnectError(err) => {
                set_conn(&state, ConnSnapshot::Error { error: err.clone() });
                emit(&app, "cp:connect-error", err);
            }
            CpUpdate::Broadcast(event) => match event {
                // Audio bytes stay Rust-side: serialising a chunk
                // through the JS boundary as a JSON number array
                // would inflate it ~5×, and JS has nothing useful
                // to do with raw PCM. Route straight to the cpal
                // playback queue instead.
                CpEvent::TtsAudio { stream_id, chunk } => {
                    state.tts_playback.enqueue(stream_id, &chunk);
                }
                // Streaming-transcription deltas: append to the
                // active PTT's running partial. The overlay polls
                // the phase cache, so the next chunk-pump tick (or
                // the manual update below) will surface the text.
                // Scoped to the live PTT — late deltas after PTT-up
                // are silently dropped.
                CpEvent::TranscriptionPartial { stream_id, text_delta } => {
                    let mut guard = state.active_ptt.lock().expect("active_ptt poisoned");
                    if let Some(p) = guard.as_mut() {
                        if p.stream_id == stream_id {
                            p.partial.push_str(&text_delta);
                        }
                    }
                }
                // Other events (including `TtsFinished`, which is
                // the workflow's cue to speak the next sentence)
                // pass through to JS as-is.
                other => emit(&app, "cp:event", other),
            },
        }
    }
}

fn set_conn(state: &State<'_, AppState>, snapshot: ConnSnapshot) {
    *state.conn.lock().expect("conn mutex poisoned") = snapshot;
}

fn emit<P: serde::Serialize + Clone>(app: &AppHandle, name: &str, payload: P) {
    if let Err(e) = app.emit(name, payload) {
        warn!(event = name, error = %e, "tauri emit failed");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let settings = DesktopSettings::load();
    // Own a multi-threaded tokio runtime for the CP/WS workers. Tauri
    // has its own async runtime for command futures, but the CP client
    // spawns long-lived tasks that should outlive any single command —
    // keep them on a dedicated handle the app owns for its lifetime.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let tokio = runtime.handle().clone();
    let cfg = settings
        .active()
        .and_then(|p| profile_to_config(p).ok().flatten());
    let initial_conn = match cfg {
        Some(_) => ConnSnapshot::Connecting,
        None => ConnSnapshot::NoConfig,
    };
    let (evt_tx, evt_rx) = mpsc::unbounded_channel::<CpUpdate>();
    let cp = CpClient::connect(&tokio, cfg, evt_tx.clone());

    // The global-shortcut Tauri plugin is always registered: even on
    // Wayland it stays inert (its handler returns early when the
    // backend is `Portal`), and registering it unconditionally lets
    // us cleanly fall back to the plugin path if portal start fails
    // — without registration here, a portal failure would leave
    // hotkeys silently dead.
    #[cfg(target_os = "linux")]
    let prefer_portal = keybind_portal::is_wayland();
    #[cfg(not(target_os = "linux"))]
    let prefer_portal = false;

    let audio_input = settings.audio.input.clone();
    let audio_output = settings.audio.output.clone();
    let state = AppState {
        tokio: tokio.clone(),
        cp: Mutex::new(cp),
        evt_tx,
        pending: Mutex::new(HashMap::new()),
        next_request_id: AtomicU64::new(1),
        settings: Mutex::new(settings),
        bundles: BundleCache::new(),
        bridges: Mutex::new(HashMap::new()),
        conn: Mutex::new(initial_conn),
        audio: AudioHandle::new(audio_input),
        active_ptt: Mutex::new(None),
        active_session: Mutex::new(None),
        keybind_backend: OnceLock::new(),
        tts_playback: TtsPlaybackHandle::new(audio_output),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(keybind::plugin())
        .manage(state)
        .register_asynchronous_uri_scheme_protocol(
            plugin_protocol::SCHEME,
            |ctx, req, responder| {
                let res = plugin_protocol::handle(ctx, req);
                responder.respond(res);
            },
        )
        .register_asynchronous_uri_scheme_protocol(
            shim_protocol::SCHEME,
            |ctx, req, responder| {
                let res = shim_protocol::handle(ctx, req);
                responder.respond(res);
            },
        )
        .setup(move |app| {
            let handle = app.handle().clone();
            // Bind the bundle cache to the app's per-user cache dir.
            // path().app_cache_dir() returns the platform-correct
            // base; failures here mean we can't load any plugins, so
            // surface them by panicking — there's nothing useful the
            // app can do without plugin storage.
            let cache_root = handle
                .path()
                .app_cache_dir()
                .expect("resolve app cache dir");
            handle
                .state::<AppState>()
                .bundles
                .init(cache_root)
                .expect("init bundle cache");
            // Register the user's keybinds (or the seeded default).
            // Parse leniently at startup — a bad combo in the
            // persisted file shouldn't disable every other hotkey, and
            // there's no user-facing call site to return the error to.
            let initial_binds = handle
                .state::<AppState>()
                .settings
                .lock()
                .expect("settings mutex poisoned")
                .effective_keybinds();
            // Sanity-check the overlay window exists. If this logs
            // "missing", the multi-window config in tauri.conf.json
            // wasn't picked up — usually because the Vite dev server
            // wasn't restarted after `rollupOptions.input` changed.
            match handle.get_webview_window("overlay") {
                Some(_) => tracing::info!("overlay window registered"),
                None => warn!(
                    "overlay window not registered — restart `tauri dev` after vite.config / tauri.conf.json changes"
                ),
            }
            // Auto-open devtools on the main window in debug builds so
            // Ctrl+Shift+I isn't needed. WebKitGTK on Linux doesn't
            // bind that shortcut, and the right-click context menu
            // only appears with the `devtools` Cargo feature on.
            #[cfg(debug_assertions)]
            if let Some(win) = handle.get_webview_window("main") {
                win.open_devtools();
            }
            let backend = build_keybind_backend(&handle, prefer_portal, &initial_binds);
            handle
                .state::<AppState>()
                .keybind_backend
                .set(backend)
                .map_err(|_| ())
                .expect("keybind_backend already initialised");
            tokio.spawn(drain_updates(handle, evt_rx));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            cp_send,
            cp_status,
            settings_get,
            settings_set,
            workflow_open_plugin,
            workflow_session_open,
            workflow_session_request,
            workflow_session_subscribe,
            workflow_session_close,
            set_active_session,
            keybind_backend,
            overlay::overlay_test,
            overlay::overlay_current_phase,
            tts_dispatch::tts_ensure_backend,
            tts_dispatch::tts_open_stream,
            tts_dispatch::tts_speak,
            tts_dispatch::tts_cancel,
            tts_dispatch::tts_close_stream,
            audio_input_devices,
            audio_output_devices,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
