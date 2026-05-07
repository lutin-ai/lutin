//! XDG Desktop Portal `GlobalShortcuts` backend (Wayland).
//!
//! Wayland forbids unprivileged X11-style global key grabs, so the
//! `tauri-plugin-global-shortcut` path silently no-ops on compositors
//! like Hyprland/Sway. The portal flips the model: we don't own the
//! key binding, the *compositor* does. We hand it a list of stable
//! shortcut ids + display names; the compositor decides which keys
//! actually fire each id, and signals `Activated`/`Deactivated` back
//! to us over D-Bus.
//!
//! Hyprland config example:
//!   bind = , Numpad1, global, lutin:lutin-kb-0
//!
//! That binds Numpad1 to whatever our `lutin-kb-0` id maps to in
//! `DesktopSettings.keybinds`. Index-based ids keep the contract
//! stable across rename; the user re-binds in their compositor only
//! when they add or remove rows.
//!
//! Reconciliation on `settings_set` recreates the session —
//! `bind_shortcuts` is one-shot per session in the portal spec, so
//! the safe path is teardown-and-reopen.

use std::thread;

use ashpd::desktop::global_shortcuts::{
    BindShortcutsOptions, GlobalShortcuts, NewShortcut,
};
use futures_util::StreamExt;
use tauri::{AppHandle, Runtime};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::dispatch::{self, Trigger};
use crate::keybind::ShortcutPhase;
use crate::settings::{Action, KeyBind, Target};

/// Stable id passed to the portal for the bind at index `idx`. The
/// compositor binds an actual key combo to this id; we resolve it
/// back to the `KeyBind` row at signal time.
pub fn portal_id_for(idx: usize) -> String {
    format!("lutin-kb-{idx}")
}

pub const PORTAL_ID_PREFIX: &str = "lutin-kb-";

/// Commands the runtime task accepts. Reconcile recreates the portal
/// session with a new bind list.
enum BackendCmd {
    Reconcile {
        binds: Vec<KeyBind>,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Handle to the portal background thread. Held by `AppState` for the
/// app's lifetime. Cloning is cheap (one mpsc clone).
#[derive(Clone)]
pub struct PortalBackend {
    cmd_tx: mpsc::UnboundedSender<BackendCmd>,
}

impl PortalBackend {
    /// Spawn the portal thread and wait for the initial session to
    /// register. Returns once `bind_shortcuts` has resolved (the
    /// user-visible "shortcuts configured" point on Hyprland).
    pub fn start<R: Runtime>(
        app: AppHandle<R>,
        initial: Vec<KeyBind>,
    ) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<BackendCmd>();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(0);

        // Own a dedicated runtime: zbus's proxy tasks need a live
        // reactor for the lifetime of the session, and Tauri's
        // command runtime is too short-lived to host them.
        thread::Builder::new()
            .name("lutin-portal-shortcuts".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .thread_name("lutin-portal-rt")
                    .build()
                    .expect("build portal tokio runtime");
                rt.block_on(run(app, initial, cmd_rx, ready_tx));
            })
            .map_err(|e| format!("spawn portal thread: {e}"))?;

        ready_rx
            .recv()
            .map_err(|_| "portal thread exited before ready signal".to_string())??;

        Ok(Self { cmd_tx })
    }

    /// Replace the registered shortcut set with `binds`. Resolves
    /// once the new session has finished `bind_shortcuts`.
    pub async fn reconcile(&self, binds: Vec<KeyBind>) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(BackendCmd::Reconcile { binds, reply: tx })
            .map_err(|_| "portal task gone".to_string())?;
        rx.await.map_err(|_| "portal reply dropped".to_string())?
    }
}

/// Task entry. Opens the session for `initial`, then loops on three
/// inputs: reconcile commands, Activated signals, Deactivated
/// signals. Reconcile recreates the inner state by reopening; the
/// surrounding loop is unaffected.
async fn run<R: Runtime>(
    app: AppHandle<R>,
    initial: Vec<KeyBind>,
    mut cmd_rx: mpsc::UnboundedReceiver<BackendCmd>,
    ready_tx: std::sync::mpsc::SyncSender<Result<(), String>>,
) {
    let mut state = match open_session(initial).await {
        Ok(s) => {
            let _ = ready_tx.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return; };
                match cmd {
                    BackendCmd::Reconcile { binds, reply } => {
                        match open_session(binds).await {
                            Ok(s) => {
                                state = s;
                                let _ = reply.send(Ok(()));
                            }
                            Err(e) => {
                                warn!(error = %e, "portal reconcile failed; keeping previous session");
                                let _ = reply.send(Err(e));
                            }
                        }
                    }
                }
            }
            Some(activated) = state.activated.next() => {
                handle_signal(&app, &state.binds, activated.shortcut_id(), ShortcutPhase::Down);
            }
            Some(deactivated) = state.deactivated.next() => {
                handle_signal(&app, &state.binds, deactivated.shortcut_id(), ShortcutPhase::Up);
            }
        }
    }
}

fn handle_signal<R: Runtime>(
    app: &AppHandle<R>,
    binds: &[KeyBind],
    id: &str,
    phase: ShortcutPhase,
) {
    let Some((action, target)) = lookup(binds, id) else {
        debug!(%id, ?phase, "portal: unknown shortcut id");
        return;
    };
    dispatch::dispatch(app.clone(), Trigger::Hotkey, action, target, phase);
}

fn lookup(binds: &[KeyBind], id: &str) -> Option<(Action, Target)> {
    let idx: usize = id.strip_prefix(PORTAL_ID_PREFIX)?.parse().ok()?;
    let bind = binds.get(idx)?;
    Some((bind.action.clone(), bind.target.clone()))
}

/// One live session + its associated proxy + the bind list it was
/// opened with. Streams keep references into the proxy, so all four
/// stay together and travel as a single unit through reconcile.
struct SessionState<A, D>
where
    A: futures_core::Stream<Item = ashpd::desktop::global_shortcuts::Activated> + Unpin,
    D: futures_core::Stream<Item = ashpd::desktop::global_shortcuts::Deactivated> + Unpin,
{
    // `_proxy` and `_session` are kept solely to extend the lifetime
    // of `activated` / `deactivated`; ashpd ties signal streams to
    // the proxy's lifetime. Dropping them detaches the D-Bus subs.
    _proxy: GlobalShortcuts,
    _session: ashpd::desktop::Session<GlobalShortcuts>,
    binds: Vec<KeyBind>,
    activated: A,
    deactivated: D,
}

async fn open_session(
    binds: Vec<KeyBind>,
) -> Result<
    SessionState<
        impl futures_core::Stream<Item = ashpd::desktop::global_shortcuts::Activated>
            + Unpin
            + use<>,
        impl futures_core::Stream<Item = ashpd::desktop::global_shortcuts::Deactivated>
            + Unpin
            + use<>,
    >,
    String,
> {
    let proxy = GlobalShortcuts::new()
        .await
        .map_err(|e| format!("portal proxy: {e}"))?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|e| format!("portal session: {e}"))?;

    let shortcuts: Vec<NewShortcut> = binds
        .iter()
        .enumerate()
        .map(|(i, b)| NewShortcut::new(portal_id_for(i), describe_bind(b)))
        .collect();

    proxy
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await
        .map_err(|e| format!("bind_shortcuts: {e}"))?
        .response()
        .map_err(|e| format!("bind_shortcuts response: {e}"))?;

    let activated = proxy
        .receive_activated()
        .await
        .map_err(|e| format!("subscribe Activated: {e}"))?;
    let deactivated = proxy
        .receive_deactivated()
        .await
        .map_err(|e| format!("subscribe Deactivated: {e}"))?;

    debug!(count = binds.len(), "portal session opened");

    Ok(SessionState {
        _proxy: proxy,
        _session: session,
        binds,
        activated,
        deactivated,
    })
}

/// Human-readable label sent to the portal. Hyprland surfaces this in
/// `hyprctl globalshortcuts` and any future "configure shortcut" UI
/// the compositor wires up.
fn describe_bind(b: &KeyBind) -> String {
    let action = match &b.action {
        Action::Ptt => "PTT",
    };
    let target = match &b.target {
        Target::ActiveWorkflow => "active workflow".to_string(),
        Target::Workflow { workflow } => format!("workflow {}", workflow.as_str()),
        Target::Clipboard => "clipboard".to_string(),
    };
    if b.combo.trim().is_empty() {
        format!("{action} → {target}")
    } else {
        format!("{action} → {target} (suggested: {})", b.combo)
    }
}

/// True when the process appears to be running on a Wayland session.
/// Read at startup to pick the backend; session type doesn't change
/// mid-process so a one-shot read is enough.
pub fn is_wayland() -> bool {
    if let Ok(t) = std::env::var("XDG_SESSION_TYPE") {
        if t == "wayland" {
            return true;
        }
    }
    std::env::var("WAYLAND_DISPLAY").is_ok()
}
