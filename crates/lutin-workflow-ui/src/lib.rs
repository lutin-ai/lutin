//! Trait surface for workflow UI plugins loaded by the desktop chrome.
//!
//! A workflow ships its UI as a `cdylib` exporting a single
//! `create_workflow` factory. The desktop chrome `dlopen`s the lib,
//! calls the factory once per opened project, and asks the resulting
//! `Workflow` to render itself into one of four fixed slots.
//!
//! IO is kept out of the trait: chrome owns WebSocket lifecycle and
//! hands every workflow instance a `Transport` of paired mpsc channels
//! carrying postcard-encoded protocol bytes. Workflows decode/encode
//! against their own protocol crates (e.g. `chat::ChatRequest`,
//! `lutin_project_protocol::Event`).
//!
//! Same-toolchain assumption: workflow `.so`s must be rebuilt whenever
//! the desktop binary or any shared `lutin-*` crate is rebuilt.

pub use egui;

use std::net::SocketAddr;

use lutin_ids::{SessionId, Slug, WorkflowId};
use tokio::sync::mpsc;

/// Fixed UI regions chrome lays out and workflows draw inside. Closed
/// enum on purpose — adding a slot is a breaking change for every
/// workflow author.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    LeftSidebar,
    TopBar,
    RightSidebar,
    Main,
}

/// Discoverable bits chrome needs *before* it asks a workflow to
/// render anything (e.g. to decide whether to allocate a right
/// sidebar at all).
#[derive(Debug, Clone)]
pub struct Manifest {
    pub display_name: String,
    pub icon: char,
    pub wants_right_sidebar: bool,
}

/// Opaque bearer token used to authenticate to a tier-2/3 endpoint.
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

/// Where a project subprocess listens, and the token chrome presents
/// when connecting. UI-side mirror of
/// `lutin_control_protocol::ProjectEndpoint`; kept independent so this
/// crate doesn't pull the full control-tier protocol surface.
#[derive(Debug, Clone)]
pub struct ProjectEndpoint {
    pub slug: Slug,
    /// The workflow id this UI was loaded as. Chrome is the single
    /// source of truth — workflows must not re-parse the literal name.
    pub workflow: WorkflowId,
    pub addr: SocketAddr,
    pub token: AuthToken,
}

/// Where a workflow session listens, and the token chrome presents
/// when connecting. UI-side mirror of
/// `lutin_project_protocol::SessionEndpoint`.
#[derive(Debug, Clone)]
pub struct SessionEndpoint {
    pub project: Slug,
    pub workflow: WorkflowId,
    pub session: SessionId,
    pub addr: SocketAddr,
    pub token: AuthToken,
}

/// Bidirectional byte stream chrome hands to a workflow on
/// construction. Each message is a full postcard-encoded
/// `lutin_protocol::Frame`: `Frame::Payload` for request/response
/// (request_id is the workflow's responsibility) and
/// `Frame::Broadcast` for server-pushed events. Chrome handles
/// `Hello` / `HelloAck` / `Ping` / `Pong` itself and never forwards
/// those.
///
/// Chrome owns the WebSocket and the pump that bridges WS frames to
/// these channels. Dropping the `Transport` (because the workflow UI
/// dropped) closes both halves; chrome notices and tears down the WS.
pub struct Transport {
    pub send: mpsc::UnboundedSender<Vec<u8>>,
    pub recv: mpsc::UnboundedReceiver<Vec<u8>>,
}

/// Back-channel actions chrome exposes to workflows. These are calls
/// that don't ride over the workflow's own protocol — they target
/// chrome itself (open a session tab, raise a notification).
pub trait ChromeApi: Send + Sync {
    /// Ask chrome to start a fresh session of `workflow` in `project`.
    /// Chrome forwards to the project tier's `StartSession`.
    fn start_session(&self, project: &Slug, workflow: &WorkflowId);
    /// Bring an existing session to the foreground in chrome's tab/UI.
    fn activate_session(&self, project: &Slug, session: &SessionId);
    /// Show a transient notification in chrome's notification area.
    fn post_notification(&self, body: &str);
}

/// Render-time context for project-scoped UI. Borrowed for the
/// duration of one `render` call; workflows must not stash it.
pub struct ProjectCtx<'a> {
    pub chrome: &'a dyn ChromeApi,
    pub slug: &'a Slug,
    /// Currently focused session in chrome's tab UI, if any. Workflow
    /// uses this to highlight the corresponding sidebar entry.
    pub active_session: Option<&'a SessionId>,
}

/// Render-time context for session-scoped UI. Workflows draw into
/// `Slot::Main` only; chrome guarantees `render` is not called for
/// other slots when this context type is used.
pub struct SessionCtx<'a> {
    pub chrome: &'a dyn ChromeApi,
    pub slug: &'a Slug,
    pub session: &'a SessionId,
}

/// Project-scoped UI. One instance per `(open project, workflow)`
/// pair. Owns subscriptions to project broadcasts via its `Transport`.
pub trait WorkflowProjectUi: Send {
    fn render(&mut self, slot: Slot, ctx: ProjectCtx<'_>, ui: &mut egui::Ui);
}

/// Session-scoped UI. One instance per active session.
pub trait WorkflowSessionUi: Send {
    fn render(&mut self, slot: Slot, ctx: SessionCtx<'_>, ui: &mut egui::Ui);
}

/// The cdylib's primary export. Chrome instantiates one `Workflow` per
/// open project and reuses it to mint per-scope UI handles.
pub trait Workflow: Send + Sync {
    fn manifest(&self) -> Manifest;
    fn open_project(
        &self,
        endpoint: ProjectEndpoint,
        transport: Transport,
    ) -> Box<dyn WorkflowProjectUi>;
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
