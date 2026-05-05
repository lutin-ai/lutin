//! Trait surface for workflow UI plugins loaded by the desktop chrome.
//!
//! A workflow ships its UI as a `cdylib` exporting a single
//! `create_workflow` factory. The desktop chrome `dlopen`s the lib,
//! calls the factory once per opened project, and asks the resulting
//! `Workflow` to mint a session UI per active session. Project-scoped
//! chrome (sidebar header, top-bar label, "+ New" button) lives in
//! desktop now — the workflow only owns the Main pane of one session.
//!
//! IO is kept out of the trait: chrome owns WebSocket lifecycle and
//! hands every session a `Transport` of paired mpsc channels carrying
//! postcard-encoded protocol bytes. Workflows decode/encode against
//! their own protocol crates (e.g. `chat::ChatRequest`).
//!
//! Same-toolchain assumption: workflow `.so`s must be rebuilt whenever
//! the desktop binary or any shared `lutin-*` crate is rebuilt.
//!
//! Spawning across the FFI boundary: each cdylib statically links its
//! own copy of tokio with a separate set of statics (runtime TLS, lazy
//! registries). Calling `tokio::spawn` from cdylib code panics because
//! its TLS is empty; calling `Handle::spawn` *appears* to work but
//! still mutates cdylib-side tokio statics on every call, which has
//! tripped state-dependent UB in practice (first session OK, second
//! segfaults). The contract is therefore: the cdylib hands `Future`s
//! to a `Spawner` trait object whose impl lives in chrome's
//! compilation unit, so `tokio::spawn` only ever runs against
//! desktop's tokio statics.

pub use egui;

use std::any::TypeId;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use lutin_ids::{SessionId, Slug, WorkflowId};
use tokio::sync::mpsc;

/// Diagnostic probe: returns the `TypeId` of a few common types, as
/// observed by the *caller's* compilation unit. Two compilations of
/// this crate (one in the desktop binary, one in a workflow cdylib)
/// each produce their own values — comparing them tells us whether
/// the two sides agree on `TypeId` across the FFI boundary, which
/// determines whether `Any`-based APIs (egui plugins, etc.) can work.
///
/// The function is defined `inline(never)` so each call site in each
/// compilation unit emits its own copy. Returns `(u64_id, ctx_id,
/// probe_id)`.
#[inline(never)]
pub fn typeid_probe() -> (TypeId, TypeId, TypeId) {
    struct Probe;
    (
        TypeId::of::<u64>(),
        TypeId::of::<egui::Context>(),
        TypeId::of::<Probe>(),
    )
}

/// Opaque bearer token used to authenticate to a session endpoint.
/// `Display` and `Debug` redact the contents to avoid leaking
/// credentials into logs.
#[derive(Clone)]
pub struct AuthToken(String);

impl AuthToken {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuthToken(<redacted>)")
    }
}

/// Where a workflow session listens, and the token chrome presents
/// when connecting. UI-side mirror of
/// `lutin_control_protocol::SessionEndpoint`.
#[derive(Debug, Clone)]
pub struct SessionEndpoint {
    pub project: Slug,
    pub workflow: WorkflowId,
    pub session: SessionId,
    pub addr: SocketAddr,
    pub token: AuthToken,
}

/// Boxed `Send` future the workflow hands to a `Spawner`.
pub type WorkflowFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Spawn a workflow-owned future onto chrome's tokio runtime. The
/// impl lives in chrome's compilation unit so the actual
/// `tokio::spawn` call runs against desktop's tokio statics — see
/// the crate-level doc comment for why the cdylib must not touch
/// tokio runtime APIs itself.
pub trait Spawner: Send + Sync {
    fn spawn(&self, fut: WorkflowFuture);
}

/// Bidirectional byte stream chrome hands to a workflow on
/// construction. Each message is a full postcard-encoded
/// `lutin_protocol::Frame`: `Frame::Payload` for request/response
/// (request_id is the workflow's responsibility) and
/// `Frame::Broadcast` for server-pushed events. Chrome handles
/// `Hello` / `HelloAck` / `Ping` / `Pong` itself and never forwards
/// those.
///
/// `spawner` is the chrome-supplied entry point for spawning the
/// workflow's pump task. Workflows must route every long-lived
/// future through it instead of calling `tokio::spawn` (or
/// `Handle::spawn`) themselves — see the crate-level doc.
///
/// Chrome owns the WebSocket and the pump that bridges WS frames to
/// these channels. Dropping the `Transport` (because the workflow UI
/// dropped) closes both halves; chrome notices and tears down the WS.
pub struct Transport {
    pub send: mpsc::UnboundedSender<Vec<u8>>,
    pub recv: mpsc::UnboundedReceiver<Vec<u8>>,
    pub spawner: Arc<dyn Spawner>,
}

/// Back-channel actions chrome exposes to workflows. These are calls
/// that don't ride over the workflow's own protocol — they target
/// chrome itself (raise a notification, refocus a session tab).
pub trait ChromeApi: Send + Sync {
    /// Bring an existing session to the foreground in chrome's tab/UI.
    fn activate_session(&self, project: &Slug, session: &SessionId);
    /// Show a transient notification in chrome's notification area.
    fn post_notification(&self, body: &str);
}

/// Render-time context for session-scoped UI. Borrowed for the
/// duration of one `render` call; workflows must not stash it.
pub struct SessionCtx<'a> {
    pub chrome: &'a dyn ChromeApi,
    pub slug: &'a Slug,
    pub session: &'a SessionId,
}

/// Session-scoped UI. One instance per active session. Renders into
/// the chrome's Main pane only — sidebar/top-bar/right-bar are owned
/// by chrome.
pub trait WorkflowSessionUi: Send {
    fn render(&mut self, ctx: SessionCtx<'_>, ui: &mut egui::Ui);
}

/// The cdylib's primary export. Chrome instantiates one `Workflow`
/// per workflow image and reuses it to mint per-session UI handles.
pub trait Workflow: Send + Sync {
    fn open_session(
        &self,
        endpoint: SessionEndpoint,
        transport: Transport,
    ) -> Box<dyn WorkflowSessionUi>;
}

/// Type of the `extern "Rust" fn create_workflow() -> Box<dyn Workflow>`
/// symbol every workflow cdylib must export. Chrome looks this up by
/// name after `dlopen`.
pub type CreateWorkflowFn = extern "Rust" fn() -> Box<dyn Workflow>;

/// Symbol name chrome resolves in each workflow `.so`. Kept as a
/// constant so chrome and workflows can't drift.
pub const CREATE_WORKFLOW_SYMBOL: &[u8] = b"create_workflow";
