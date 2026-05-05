//! Tauri chrome entry point: owns the CP client, wires Tauri commands
//! that JS calls, and pumps `CpUpdate` events out as Tauri events the
//! React chrome listens to.

mod cp;
mod settings;

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use lutin_control_protocol::{Request, Response};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;
use tracing_subscriber::EnvFilter;

use crate::cp::{CpClient, CpCommand, CpConfig, CpUpdate, RequestId, Token};
use crate::settings::{ConnectionProfile, DesktopSettings};

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
    /// Last known connection state, updated by the drainer task.
    /// Read by `cp_status` so JS can initialize without racing
    /// against a `cp:connected` event that fires before the
    /// listener attaches.
    conn: Mutex<ConnSnapshot>,
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

#[tauri::command]
async fn cp_send(state: State<'_, AppState>, request: Request) -> Result<Response, String> {
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
        return Err("control panel not connected".into());
    }
    rx.await.map_err(|_| "request cancelled".to_string())
}

#[tauri::command]
fn cp_status(state: State<'_, AppState>) -> ConnSnapshot {
    state.conn.lock().expect("conn mutex poisoned").clone()
}

#[tauri::command]
fn settings_get(state: State<'_, AppState>) -> DesktopSettings {
    state.settings.lock().expect("settings mutex poisoned").clone()
}

#[tauri::command]
fn settings_set(state: State<'_, AppState>, new: DesktopSettings) -> Result<(), String> {
    new.save()?;
    let cfg = match new.active() {
        Some(p) => profile_to_config(p)?,
        None => None,
    };
    *state.settings.lock().expect("settings mutex poisoned") = new;
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
            CpUpdate::Broadcast(event) => emit(&app, "cp:event", event),
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

    let state = AppState {
        tokio: tokio.clone(),
        cp: Mutex::new(cp),
        evt_tx,
        pending: Mutex::new(HashMap::new()),
        next_request_id: AtomicU64::new(1),
        settings: Mutex::new(settings),
        conn: Mutex::new(initial_conn),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();
            tokio.spawn(drain_updates(handle, evt_rx));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            cp_send,
            cp_status,
            settings_get,
            settings_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
