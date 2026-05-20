//! Desktop chrome — top-level egui app.
//!
//! Owns the chrome layout (top bar + left sidebar with project list and
//! per-project session list + right details panel + main pane) and the
//! project picker. Workflow cdylibs only render into the Main pane via
//! one `WorkflowSessionUi` per open session; every other surface
//! (sidebar header/icon, "+ New" button, session tabs, top-bar label)
//! is owned by chrome and parameterised from `WorkflowInfo`
//! (display_name, icon) reported by CP — chrome can decorate before
//! the cdylib has been dlopened.
//!
//! All per-opened-project state lives in `App::projects_state` keyed
//! by `Slug`. One entry owns the session list, every loaded session
//! UI, and the focused session id. Dropping the entry tears down the
//! session UIs and their WS bridges in the right order via Drop.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align, CentralPanel, Color32, Layout, RichText};
use lutin_control_protocol::{
    DisplayName, DisplayNameError, Event as CpEvent, ProjectInfo, Request, Response, ResponseOk,
    SessionEndpoint as CpSessionEndpoint, SessionInfo, Slug, WorkflowInfo,
};
use lutin_ids::{SessionId, SlugError, WorkflowId};
use lutin_ui::prelude::*;
use lutin_ui::widget::{button, panel, text_input};
use lutin_workflow_ui::{
    AuthToken, ChromeApi, SessionCtx, SessionEndpoint as UiSessionEndpoint, WorkflowSessionUi,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::bridge::{self, ChromeSpawner, make_transport_pair};
use crate::cp::{CpClient, CpCommand, CpConfig, CpUpdate, RequestId, Token};
use crate::loader::{WorkflowCache, WorkflowLibrary};
use crate::settings::DesktopSettings;
use crate::view::settings::{self as settings_view, ConnStatus, NewConnectionForm};

/// Top-level chrome view. `Projects` is the normal workspace; `Settings`
/// is the desktop-local settings editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Projects,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    Disconnected,
    Rejected(String),
    Error(String),
}

impl ConnState {
    fn label(&self) -> (&'static str, Color32) {
        let t = theme();
        match self {
            ConnState::Connecting => ("connecting…", t.text.dim),
            ConnState::Connected => ("connected", t.accent.bright),
            ConnState::Disconnected => ("disconnected", t.text.dim),
            ConnState::Rejected(_) => ("rejected", t.status.error.solid),
            ConnState::Error(_) => ("error", t.status.error.solid),
        }
    }

    fn is_connecting(&self) -> bool {
        matches!(self, ConnState::Connecting)
    }

    fn is_connected(&self) -> bool {
        matches!(self, ConnState::Connected)
    }

    /// Reason tail for `Rejected`/`Error` (the "why" the user wants
    /// to read on the Settings card). `None` for the other variants.
    fn detail(&self) -> Option<&str> {
        match self {
            ConnState::Rejected(reason) | ConnState::Error(reason) => Some(reason.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Disconnected;

impl std::fmt::Display for Disconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "disconnected from control-panel")
    }
}

impl std::error::Error for Disconnected {}

pub enum Intent {
    SelectProject(Slug),
    OpenProject(Slug),
    DeleteProject(Slug),
    SubmitCreate {
        slug: Slug,
        display_name: DisplayName,
    },
    EditNewSlug(String),
    EditNewDisplay(String),
    SetFormError(String),
    ClearFormError,
    StartSession {
        slug: Slug,
        workflow: WorkflowId,
    },
    ActivateSession {
        slug: Slug,
        session: SessionId,
    },
    /// Drop a session UI (close its tab) without ending the session
    /// upstream; the session continues running on the project tier.
    CloseSession {
        slug: Slug,
        session: SessionId,
    },
}

fn default_workflow_id() -> WorkflowId {
    WorkflowId::parse("chat").expect("hardcoded id is valid")
}

/// True when the active connection is at least *shaped* like one we
/// could dial — non-empty addr and token. Doesn't validate the addr
/// parses or the token verifies; that's `main`'s job. Used to pick
/// the initial view: an unusable settings entry forces Settings.
fn connection_usable(settings: &DesktopSettings) -> bool {
    settings
        .active()
        .is_some_and(|c| !c.addr.trim().is_empty() && !c.token.trim().is_empty())
}

struct LoadedSession {
    ui: Box<dyn WorkflowSessionUi>,
    /// Aborts the session's WS bridge task on drop.
    _bridge: BridgeGuard,
    _lib: Arc<WorkflowLibrary>,
}

/// Aborts a tokio task on drop.
struct BridgeGuard(tokio::task::AbortHandle);

impl Drop for BridgeGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Everything chrome owns about one opened project. Dropping the
/// entry tears down every loaded session UI (and its WS bridge).
struct ProjectEntry {
    /// Sessions reported by CP (mirrors `ListSessions` +
    /// `SessionStarted`/`SessionEnded` broadcasts).
    sessions: Vec<SessionInfo>,
    /// Loaded session UIs keyed by session id.
    loaded_sessions: HashMap<SessionId, LoadedSession>,
    /// Currently focused session in this project's tab strip.
    active_session: Option<SessionId>,
}

pub enum ChromeIntent {
    ActivateSession {
        project: Slug,
        session: SessionId,
    },
    Notify(String),
}

/// Real `ChromeApi` impl handed to every `WorkflowSessionUi`. Calls
/// are non-blocking — they push onto an mpsc the App drains during
/// its frame loop. Cheaply cloneable (it's just a sender) so the App
/// hands a fresh clone to each render context.
#[derive(Clone)]
struct RealChromeApi {
    tx: mpsc::UnboundedSender<ChromeIntent>,
}

impl ChromeApi for RealChromeApi {
    fn activate_session(&self, project: &Slug, session: &SessionId) {
        let _ = self.tx.send(ChromeIntent::ActivateSession {
            project: project.clone(),
            session: session.clone(),
        });
    }
    fn post_notification(&self, body: &str) {
        let _ = self.tx.send(ChromeIntent::Notify(body.to_string()));
    }
}

/// Bookkeeping for in-flight CP requests whose reply needs to be
/// routed back to the originating UI action. Replies that don't need
/// follow-up context (e.g. `WorkflowCdylib`, which is self-describing)
/// don't go here.
#[derive(Debug, Clone)]
enum Pending {
    ListSessions(Slug),
    StartSession(Slug),
}

impl Pending {
    fn slug(&self) -> &Slug {
        match self {
            Pending::ListSessions(s) | Pending::StartSession(s) => s,
        }
    }
}

/// A session whose `StartSession` reply arrived before the workflow
/// cdylib did. Replayed when the cdylib install completes for the
/// matching `workflow`.
struct DeferredSession {
    slug: Slug,
    session: SessionId,
    workflow: WorkflowId,
    endpoint: CpSessionEndpoint,
}

pub struct App {
    /// Control-panel client. Owns the worker task + its channels;
    /// re-dialing on settings change goes through `cp.reconnect`.
    cp: CpClient,
    chrome_intent_rx: mpsc::UnboundedReceiver<ChromeIntent>,
    tokio: tokio::runtime::Handle,
    egui_ctx: egui::Context,

    conn: ConnState,
    /// Authoritative project list from the control-panel.
    projects: Vec<ProjectInfo>,
    /// Authoritative workflow list (refreshed on connect via
    /// `ListWorkflows`).
    workflows: Vec<WorkflowInfo>,
    /// Per-opened-project state. Single source of truth — dropping an
    /// entry tears down bridges and loaded UIs together.
    projects_state: HashMap<Slug, ProjectEntry>,
    workflow_cache: WorkflowCache,
    chrome_api: RealChromeApi,

    /// In-flight requests that need follow-up routing when the reply
    /// arrives. Replies that don't carry interesting context (e.g.
    /// `ListProjects`, `DeleteProject`) don't appear here.
    pending: HashMap<RequestId, Pending>,
    /// Workflows for which a `GetWorkflowCdylib` is in flight. Guards
    /// against firing a duplicate fetch while the first is still on
    /// the wire.
    inflight_cdylibs: HashSet<WorkflowId>,
    /// Session loads whose `StartSession` reply landed before the
    /// workflow cdylib was available. Drained when the cdylib install
    /// completes.
    pending_sessions: Vec<DeferredSession>,

    next_request_id: u64,
    /// Project currently focused in chrome's main pane.
    active: Option<Slug>,

    new_slug: String,
    new_display: String,
    new_error: Option<String>,

    last_error: Option<String>,
    /// Most recent notification text, surfaced in the top bar. C3 keeps
    /// it ephemeral (overwritten on each new notification).
    notification: Option<String>,

    view_mode: ViewMode,
    settings: DesktopSettings,
    new_connection: NewConnectionForm,
    settings_status: Option<String>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        tokio: tokio::runtime::Handle,
        workflow_cache: WorkflowCache,
        settings: DesktopSettings,
    ) -> Self {
        lutin_ui::font::install(&cc.egui_ctx, lutin_ui::font::Preset::Inter);
        set_theme(dark(), &cc.egui_ctx);

        let (chrome_intent_tx, chrome_intent_rx) = mpsc::unbounded_channel();

        let cfg = build_cp_config(&settings);
        let conn = if cfg.is_some() {
            ConnState::Connecting
        } else {
            ConnState::Disconnected
        };
        let cp = CpClient::connect(&tokio, &cc.egui_ctx, cfg);

        Self {
            cp,
            chrome_intent_rx,
            tokio,
            egui_ctx: cc.egui_ctx.clone(),
            conn,
            projects: Vec::new(),
            workflows: Vec::new(),
            projects_state: HashMap::new(),
            workflow_cache,
            chrome_api: RealChromeApi { tx: chrome_intent_tx },
            pending: HashMap::new(),
            inflight_cdylibs: HashSet::new(),
            pending_sessions: Vec::new(),
            next_request_id: 1,
            active: None,
            new_slug: String::new(),
            new_display: String::new(),
            new_error: None,
            last_error: None,
            notification: None,
            view_mode: if connection_usable(&settings) {
                ViewMode::Projects
            } else {
                ViewMode::Settings
            },
            settings,
            new_connection: NewConnectionForm::default(),
            settings_status: None,
        }
    }

    /// Persist current settings and re-dial the active connection
    /// without restarting the process. Tears down any open project
    /// state (it's tied to the previous control-panel) and respawns
    /// the cp worker against the newly-active profile. Returns a
    /// human-readable status for the Settings view.
    /// Persist the current in-memory profiles to disk. Doesn't touch
    /// the live connection — dialing is button-driven via `dial_index`.
    fn save_settings(&mut self) -> String {
        match self.settings.save() {
            Ok(()) => "Saved.".into(),
            Err(e) => format!("Save failed: {e}"),
        }
    }

    /// Make the profile at `index` the active one, persist, and dial
    /// it. Tears down everything tied to the prior control-panel
    /// (loaded projects + their workers/bridges, pending request
    /// bookkeeping) before respawning the cp worker.
    fn dial_index(&mut self, index: usize) -> String {
        let Some(profile) = self.settings.connections.get(index) else {
            warn!(index, "dial_index: no profile at that index");
            return "No such connection.".into();
        };
        info!(name = %profile.name, addr = %profile.addr, "dialing control-panel");
        self.settings.default = profile.name.clone();
        if let Err(e) = self.settings.save() {
            return format!("Save failed: {e}");
        }

        self.projects_state.clear();
        self.projects.clear();
        self.workflows.clear();
        self.pending.clear();
        self.inflight_cdylibs.clear();
        self.pending_sessions.clear();
        self.active = None;
        self.last_error = None;

        let cfg = build_cp_config(&self.settings);
        let has_cfg = cfg.is_some();
        self.cp.reconnect(&self.tokio, &self.egui_ctx, cfg);
        if has_cfg {
            self.conn = ConnState::Connecting;
            "Reconnecting…".into()
        } else {
            self.conn = ConnState::Disconnected;
            "Profile is missing addr or token — fix it and try again.".into()
        }
    }

    fn next_request_id(&mut self) -> RequestId {
        let id = RequestId(self.next_request_id);
        self.next_request_id += 1;
        id
    }

    /// Open `slug` in chrome. Inserts an empty `ProjectEntry`
    /// immediately (so the rest of the UI tracks it) and asks CP for
    /// the session list. Cdylibs are dlopened lazily when a session
    /// opens — chrome's per-project decoration (icon, label, "+ New"
    /// button) reads from `WorkflowInfo` and doesn't need the cdylib.
    fn open_project(&mut self, slug: Slug) {
        self.projects_state.entry(slug.clone()).or_insert(ProjectEntry {
            sessions: Vec::new(),
            loaded_sessions: HashMap::new(),
            active_session: None,
        });
        if let Ok(id) = self.send(Request::ListSessions { slug: slug.clone() }) {
            self.pending.insert(id, Pending::ListSessions(slug));
        }
    }

    fn workflow_for(&self, id: &WorkflowId) -> Option<&WorkflowInfo> {
        self.workflows.iter().find(|w| &w.id == id)
    }

    fn request_cdylib_if_needed(&mut self, workflow: &WorkflowId) {
        if self.inflight_cdylibs.contains(workflow) {
            return;
        }
        if self.send(Request::GetWorkflowCdylib { id: workflow.clone() }).is_ok() {
            self.inflight_cdylibs.insert(workflow.clone());
        }
    }

    fn send(&mut self, req: Request) -> Result<RequestId, Disconnected> {
        let request_id = self.next_request_id();
        if self
            .cp
            .send(CpCommand::Send {
                request_id,
                request: req,
            })
            .is_err()
        {
            warn!("cp worker channel closed; command dropped");
            return Err(Disconnected);
        }
        Ok(request_id)
    }

    fn entry(&self, slug: &Slug) -> Option<&ProjectEntry> {
        self.projects_state.get(slug)
    }

    fn entry_mut(&mut self, slug: &Slug) -> Option<&mut ProjectEntry> {
        self.projects_state.get_mut(slug)
    }

    fn drain_events(&mut self) {
        while let Some(ev) = self.cp.try_recv() {
            match ev {
                CpUpdate::Connected => {
                    self.conn = ConnState::Connected;
                    self.last_error = None;
                    if let Err(e) = self.send(Request::ListProjects) {
                        self.last_error = Some(e.to_string());
                    }
                    if let Err(e) = self.send(Request::ListWorkflows) {
                        self.last_error = Some(e.to_string());
                    }
                }
                CpUpdate::Disconnected => self.conn = ConnState::Disconnected,
                CpUpdate::HandshakeRejected(reason) => self.conn = ConnState::Rejected(reason),
                CpUpdate::ConnectError(e) => self.conn = ConnState::Error(e),
                CpUpdate::Response { request_id, response } => {
                    self.on_response(request_id, response)
                }
                CpUpdate::Broadcast(ev) => self.on_broadcast(ev),
            }
        }
        self.drain_chrome_intents();
    }

    fn drain_chrome_intents(&mut self) {
        while let Ok(intent) = self.chrome_intent_rx.try_recv() {
            match intent {
                ChromeIntent::ActivateSession { project, session } => {
                    if let Some(entry) = self.entry_mut(&project) {
                        entry.active_session = Some(session);
                    }
                    self.active = Some(project);
                }
                ChromeIntent::Notify(body) => {
                    self.notification = Some(body);
                }
            }
        }
    }

    fn start_session(&mut self, slug: Slug, workflow: WorkflowId) {
        match self.send(Request::StartSession {
            slug: slug.clone(),
            workflow,
        }) {
            Ok(id) => {
                self.pending.insert(id, Pending::StartSession(slug));
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
            }
        }
    }

    fn on_response(&mut self, request_id: RequestId, resp: Response) {
        let pending = self.pending.remove(&request_id);
        match resp {
            Response::Ok(ok) => match ok {
                ResponseOk::Projects(list) => {
                    self.projects = list;
                    let known: std::collections::HashSet<Slug> =
                        self.projects.iter().map(|p| p.slug.clone()).collect();
                    if let Some(slug) = &self.active
                        && !known.contains(slug)
                    {
                        self.active = None;
                    }
                    self.projects_state.retain(|slug, _| known.contains(slug));
                    self.pending.retain(|_, p| known.contains(p.slug()));
                }
                ResponseOk::Created(_) => {
                    self.new_slug.clear();
                    self.new_display.clear();
                    self.new_error = None;
                }
                ResponseOk::Deleted => {}
                ResponseOk::Workflows(list) => {
                    self.workflows = list;
                    // Prefetch every workflow whose cdylib isn't
                    // already cached on disk for the digest CP just
                    // reported. Cheap when up-to-date — `try_load`
                    // returns the cached entry without a fetch.
                    let workflows = self.workflows.clone();
                    for w in &workflows {
                        match self.workflow_cache.try_load(&w.id, &w.digest) {
                            Ok(Some(_)) => {}
                            Ok(None) => self.request_cdylib_if_needed(&w.id),
                            Err(e) => {
                                self.last_error =
                                    Some(format!("cache probe for {}: {e}", w.id.as_str()));
                            }
                        }
                    }
                }
                ResponseOk::WorkflowCdylib { id, digest, bytes } => {
                    self.inflight_cdylibs.remove(&id);
                    if let Err(e) = self.workflow_cache.install(&id, &digest, &bytes) {
                        self.last_error = Some(format!(
                            "install cdylib for {}: {e}",
                            id.as_str()
                        ));
                        return;
                    }
                    // Drain anything that was waiting on this workflow.
                    let pending: Vec<DeferredSession> = self
                        .pending_sessions
                        .drain(..)
                        .collect();
                    let (ready, still_waiting): (Vec<_>, Vec<_>) =
                        pending.into_iter().partition(|d| d.workflow == id);
                    self.pending_sessions = still_waiting;
                    for d in ready {
                        self.load_session(d.slug, d.session, d.endpoint);
                    }
                }
                ResponseOk::Sessions(list) => {
                    let Some(Pending::ListSessions(slug)) = pending else {
                        warn!("Sessions response without pending ListSessions");
                        return;
                    };
                    let Some(entry) = self.entry_mut(&slug) else {
                        return;
                    };
                    entry.sessions = list;
                    let live: std::collections::HashSet<SessionId> =
                        entry.sessions.iter().map(|s| s.id.clone()).collect();
                    entry.loaded_sessions.retain(|sid, _| live.contains(sid));
                    if let Some(active) = &entry.active_session
                        && !live.contains(active)
                    {
                        entry.active_session = None;
                    }
                }
                ResponseOk::SessionStarted { info, endpoint } => {
                    let Some(Pending::StartSession(slug)) = pending else {
                        warn!("SessionStarted response without pending StartSession");
                        return;
                    };
                    let session_id = info.id.clone();
                    self.upsert_session(&slug, info);
                    self.load_session(slug, session_id, endpoint);
                }
                ResponseOk::SessionStopped => {}
                ResponseOk::SessionOpened(_) => {
                    warn!("SessionOpened response without pending OpenSession");
                }
            },
            Response::Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn on_broadcast(&mut self, ev: CpEvent) {
        match ev {
            CpEvent::ProjectCreated(_) | CpEvent::ProjectDeleted { .. } => {
                if let Err(e) = self.send(Request::ListProjects) {
                    self.last_error = Some(e.to_string());
                }
            }
            CpEvent::SessionStarted { slug, info } => {
                self.upsert_session(&slug, info);
            }
            CpEvent::SessionEnded { slug, session } => {
                if let Some(entry) = self.entry_mut(&slug) {
                    entry.sessions.retain(|s| s.id != session);
                    entry.loaded_sessions.remove(&session);
                    if entry.active_session.as_ref() == Some(&session) {
                        entry.active_session = None;
                    }
                }
            }
        }
    }

    fn upsert_session(&mut self, slug: &Slug, info: SessionInfo) {
        let Some(entry) = self.entry_mut(slug) else {
            return;
        };
        if !entry.sessions.iter().any(|s| s.id == info.id) {
            entry.sessions.push(info);
        }
    }

    fn load_session(
        &mut self,
        slug: Slug,
        session: SessionId,
        ep: CpSessionEndpoint,
    ) {
        let workflow_id = default_workflow_id();
        let Some(info) = self.workflow_for(&workflow_id).cloned() else {
            // Workflow list hasn't arrived yet — defer until it has.
            self.pending_sessions.push(DeferredSession {
                slug,
                session,
                workflow: workflow_id,
                endpoint: ep,
            });
            return;
        };
        let lib = match self.workflow_cache.try_load(&workflow_id, &info.digest) {
            Ok(Some(lib)) => lib,
            Ok(None) => {
                // Bytes not on disk yet; queue the load for after the
                // cdylib install lands.
                self.pending_sessions.push(DeferredSession {
                    slug,
                    session,
                    workflow: workflow_id.clone(),
                    endpoint: ep,
                });
                self.request_cdylib_if_needed(&workflow_id);
                return;
            }
            Err(e) => {
                self.last_error = Some(format!("workflow load failed: {e}"));
                return;
            }
        };

        let spawner: Arc<dyn lutin_workflow_ui::Spawner> =
            Arc::new(ChromeSpawner::new(self.tokio.clone()));
        let (transport, bridge_endpoints) = make_transport_pair(spawner);
        let token = AuthToken::new(ep.token.clone());
        let ui_endpoint = UiSessionEndpoint {
            project: slug.clone(),
            workflow: workflow_id,
            session: session.clone(),
            addr: ep.addr,
            token: token.clone(),
        };

        let bridge_task = self.tokio.spawn(bridge::run_workflow_bridge(
            slug.clone(),
            ep.addr,
            token,
            bridge_endpoints,
        ));
        let ui = lib.workflow().open_session(ui_endpoint, transport);

        let Some(entry) = self.entry_mut(&slug) else {
            return;
        };
        entry.loaded_sessions.insert(
            session.clone(),
            LoadedSession {
                ui,
                _bridge: BridgeGuard(bridge_task.abort_handle()),
                _lib: lib,
            },
        );
        entry.active_session = Some(session);
        self.active = Some(slug);
    }

    fn apply(&mut self, intents: Vec<Intent>) {
        for intent in intents {
            match intent {
                Intent::SelectProject(slug) => {
                    self.active = Some(slug);
                }
                Intent::OpenProject(slug) => {
                    self.open_project(slug);
                }
                Intent::StartSession { slug, workflow } => {
                    self.start_session(slug, workflow);
                }
                Intent::DeleteProject(slug) => {
                    if let Err(e) = self.send(Request::DeleteProject { slug }) {
                        self.last_error = Some(e.to_string());
                    }
                }
                Intent::SubmitCreate { slug, display_name } => {
                    if let Err(e) = self.send(Request::CreateProject { slug, display_name }) {
                        self.last_error = Some(e.to_string());
                    }
                }
                Intent::EditNewSlug(s) => self.new_slug = s,
                Intent::EditNewDisplay(s) => self.new_display = s,
                Intent::SetFormError(e) => self.new_error = Some(e),
                Intent::ClearFormError => self.new_error = None,
                Intent::ActivateSession { slug, session } => {
                    if let Some(entry) = self.entry_mut(&slug) {
                        entry.active_session = Some(session);
                    }
                    self.active = Some(slug);
                }
                Intent::CloseSession { slug, session } => {
                    if let Some(entry) = self.entry_mut(&slug) {
                        entry.loaded_sessions.remove(&session);
                        if entry.active_session.as_ref() == Some(&session) {
                            entry.active_session = None;
                        }
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();

        let mut intents: Vec<Intent> = Vec::new();

        egui::Panel::top("chrome-top")
            .exact_size(36.0)
            .show_inside(ui, |ui| {
                intents.extend(draw_top_bar(self, ui));
            });

        egui::Panel::left("chrome-left")
            .resizable(true)
            .default_size(220.0)
            .min_size(180.0)
            .show_inside(ui, |ui| {
                intents.extend(draw_left_sidebar(self, ui));
            });

        egui::Panel::right("chrome-right")
            .resizable(true)
            .default_size(260.0)
            .min_size(200.0)
            .show_inside(ui, |ui| {
                intents.extend(draw_right_sidebar(self, ui));
            });

        CentralPanel::default().show_inside(ui, |ui| {
            if self.view_mode == ViewMode::Settings {
                let (label, color) = self.conn.label();
                let conn_status = ConnStatus {
                    label,
                    color,
                    detail: self.conn.detail(),
                    connecting: self.conn.is_connecting(),
                    connected: self.conn.is_connected(),
                };
                let action = settings_view::show(
                    ui,
                    &mut self.settings,
                    &mut self.new_connection,
                    self.settings_status.as_deref(),
                    Some(conn_status),
                );
                if action.save_clicked {
                    self.settings_status = Some(self.save_settings());
                }
                // Connect and Reconnect both run the same dial path —
                // the labels are UX hints (Connect on inactive cards,
                // Reconnect on the currently-active one).
                if let Some(idx) = action.connect_index.or(action.reconnect_index) {
                    self.settings_status = Some(self.dial_index(idx));
                }
            } else {
                intents.extend(draw_main(self, ui));
            }
        });

        // Chrome intents emitted *during* this frame's render need to
        // affect the same frame's `apply` (so e.g. an "activate session"
        // click in the workflow's sidebar focuses the tab on this
        // frame, not the next one).
        self.drain_chrome_intents();

        self.apply(intents);
    }
}

impl App {
    /// Render the active project's session UI (if any) into the Main
    /// pane. Returns true iff a session UI rendered.
    fn render_session_main(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(slug) = self.active.clone() else {
            return false;
        };
        let chrome = self.chrome_api.clone();
        let Some(entry) = self.entry_mut(&slug) else {
            return false;
        };
        let Some(session) = entry.active_session.clone() else {
            return false;
        };
        let Some(loaded) = entry.loaded_sessions.get_mut(&session) else {
            return false;
        };
        let ctx = SessionCtx {
            chrome: &chrome,
            slug: &slug,
            session: &session,
        };
        loaded.ui.render(ctx, ui);
        true
    }

    /// Lookup `WorkflowInfo` (icon + display name) for the workflow
    /// id chrome uses for a given project. Returns `None` while CP's
    /// `ListWorkflows` reply is still in flight.
    fn workflow_info_for(&self, _slug: &Slug) -> Option<&WorkflowInfo> {
        let id = default_workflow_id();
        self.workflow_for(&id)
    }
}

fn draw_top_bar(app: &mut App, ui: &mut egui::Ui) -> Vec<Intent> {
    ui.horizontal_centered(|ui| {
        ui.label(RichText::new("lutin").strong());
        ui.add_space(12.0);
        let (label, color) = app.conn.label();
        if app.conn.is_connecting() {
            ui.add(egui::Spinner::new().size(12.0));
            ui.add_space(4.0);
        }
        ui.label(RichText::new(label).color(color).small());
        if let ConnState::Rejected(reason) | ConnState::Error(reason) = &app.conn {
            ui.label(
                RichText::new(format!("— {reason}"))
                    .color(color)
                    .small(),
            );
        }
        ui.add_space(16.0);
        if let Some(slug) = &app.active
            && let Some(info) = app.workflow_info_for(slug)
        {
            ui.label(RichText::new(format!("{} {} — {}", info.icon, info.display_name, slug)).strong());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (label, variant) = match app.view_mode {
                ViewMode::Projects => ("Settings", lutin_ui::widget::button::Variant::Ghost),
                ViewMode::Settings => ("Back", lutin_ui::widget::button::Variant::Primary),
            };
            if ui
                .add(button::ghost(label).small().variant(variant))
                .clicked()
            {
                app.view_mode = match app.view_mode {
                    ViewMode::Projects => ViewMode::Settings,
                    ViewMode::Settings => ViewMode::Projects,
                };
            }
            ui.add_space(8.0);
            if let Some(err) = &app.last_error {
                ui.label(
                    RichText::new(format!("⚠ {err}"))
                        .color(theme().text.dim)
                        .small(),
                );
            } else if let Some(note) = &app.notification {
                ui.label(RichText::new(note).color(theme().text.dim).small());
            }
        });
    });
    Vec::new()
}

fn draw_left_sidebar(app: &mut App, ui: &mut egui::Ui) -> Vec<Intent> {
    let mut intents = Vec::new();
    panel::Panel::new()
        .header("Projects")
        .show(ui, |ui| {
            if app.projects.is_empty() {
                ui.label(
                    RichText::new("no projects yet")
                        .color(theme().text.dim)
                        .small(),
                );
            } else {
                for p in &app.projects {
                    let is_active = app.active.as_ref() == Some(&p.slug);
                    ui.horizontal(|ui| {
                        let mut btn = button::ghost(p.display_name.as_str()).full_width();
                        if is_active {
                            btn = btn.variant(lutin_ui::widget::button::Variant::Primary);
                        }
                        if ui.add(btn).clicked() {
                            intents.push(Intent::SelectProject(p.slug.clone()));
                            if app.entry(&p.slug).is_none() {
                                intents.push(Intent::OpenProject(p.slug.clone()));
                            }
                        }
                    });
                }
            }
            ui.add_space(8.0);

            ui.label(
                RichText::new("New project")
                    .color(theme().text.dim)
                    .small(),
            );
            let mut slug_buf = app.new_slug.clone();
            if ui
                .add(text_input::TextInput::new(&mut slug_buf).hint("slug"))
                .changed()
            {
                intents.push(Intent::EditNewSlug(slug_buf));
            }
            let mut display_buf = app.new_display.clone();
            if ui
                .add(text_input::TextInput::new(&mut display_buf).hint("display name"))
                .changed()
            {
                intents.push(Intent::EditNewDisplay(display_buf));
            }
            if let Some(err) = &app.new_error {
                ui.label(RichText::new(err).color(theme().text.dim).small());
            }
            let can_submit = !app.new_slug.is_empty()
                && !app.new_display.is_empty()
                && app.conn == ConnState::Connected;
            let mut btn = button::primary("Create").full_width();
            if !can_submit {
                btn = btn.disabled();
            }
            if ui.add(btn).clicked() {
                match (
                    Slug::parse(app.new_slug.clone()),
                    DisplayName::parse(app.new_display.clone()),
                ) {
                    (Ok(slug), Ok(name)) => {
                        intents.push(Intent::ClearFormError);
                        intents.push(Intent::SubmitCreate {
                            slug,
                            display_name: name,
                        });
                    }
                    (Err(e), _) => intents.push(Intent::SetFormError(slug_error(&e))),
                    (_, Err(e)) => intents.push(Intent::SetFormError(display_name_error(&e))),
                }
            }
        });
    ui.add_space(12.0);
    ui.separator();
    intents.extend(draw_project_sessions_panel(app, ui));
    intents
}

/// Per-project session list + "+ New" button for the active project.
/// Lives in the left sidebar under the projects panel; chrome owns
/// this directly (the workflow cdylib only renders the Main pane).
fn draw_project_sessions_panel(app: &App, ui: &mut egui::Ui) -> Vec<Intent> {
    let mut intents = Vec::new();
    let Some(slug) = app.active.clone() else {
        return intents;
    };
    let info = app.workflow_info_for(&slug);
    let workflow_id = default_workflow_id();
    let header = match info {
        Some(i) => format!("{} {}", i.icon, i.display_name),
        None => "Sessions".to_owned(),
    };
    panel::Panel::new()
        .header(header.as_str())
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(button::primary("+ New").small()).clicked() {
                        intents.push(Intent::StartSession {
                            slug: slug.clone(),
                            workflow: workflow_id.clone(),
                        });
                    }
                });
            });
            ui.add_space(4.0);
            let Some(entry) = app.entry(&slug) else {
                return;
            };
            if entry.sessions.is_empty() {
                ui.label(
                    RichText::new("no sessions yet")
                        .color(theme().text.dim)
                        .small(),
                );
                return;
            }
            for s in &entry.sessions {
                let is_active = entry.active_session.as_ref() == Some(&s.id);
                let mut btn = button::ghost(s.id.to_string()).full_width();
                if is_active {
                    btn = btn.variant(lutin_ui::widget::button::Variant::Primary);
                }
                if ui.add(btn).clicked() {
                    intents.push(Intent::ActivateSession {
                        slug: slug.clone(),
                        session: s.id.clone(),
                    });
                }
            }
        });
    intents
}

fn draw_right_sidebar(app: &mut App, ui: &mut egui::Ui) -> Vec<Intent> {
    panel::Panel::new().header("Details").show(ui, |ui| {
        let Some(slug) = &app.active else {
            ui.label(
                RichText::new("no project selected")
                    .color(theme().text.dim)
                    .small(),
            );
            return;
        };
        let Some(p) = app.projects.iter().find(|p| &p.slug == slug) else {
            return;
        };
        ui.label(RichText::new(p.display_name.as_str()).strong());
        ui.label(
            RichText::new(format!("slug: {}", p.slug))
                .color(theme().text.dim)
                .small(),
        );
        ui.add_space(8.0);
        if let Some(entry) = app.entry(slug) {
            ui.label(
                RichText::new(format!("sessions: {}", entry.sessions.len()))
                    .color(theme().text.dim)
                    .small(),
            );
        }
    });
    Vec::new()
}

fn draw_main(app: &mut App, ui: &mut egui::Ui) -> Vec<Intent> {
    let mut intents = Vec::new();
    let Some(slug) = app.active.clone() else {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.label(RichText::new("Welcome").size(20.0).strong());
            ui.label(
                RichText::new("Pick a project from the sidebar, or create one.")
                    .color(theme().text.dim),
            );
        });
        return intents;
    };

    intents.extend(draw_session_tabs(app, &slug, ui));
    ui.separator();

    if app.render_session_main(ui) {
        return intents;
    }

    let info = app.projects.iter().find(|p| p.slug == slug).cloned();
    ui.vertical(|ui| {
        if let Some(p) = &info {
            ui.label(RichText::new(p.display_name.as_str()).size(18.0).strong());
            ui.add_space(12.0);
        }
        ui.label(
            RichText::new("Pick a session from the sidebar, or start a new one.")
                .color(theme().text.dim),
        );
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.add(button::danger("Delete")).clicked()
                && let Some(p) = &info
            {
                intents.push(Intent::DeleteProject(p.slug.clone()));
            }
        });
    });
    intents
}

fn draw_session_tabs(app: &App, slug: &Slug, ui: &mut egui::Ui) -> Vec<Intent> {
    let mut intents = Vec::new();
    let Some(entry) = app.entry(slug) else {
        return intents;
    };
    let active = entry.active_session.as_ref();
    ui.horizontal(|ui| {
        if entry.sessions.is_empty() {
            ui.label(
                RichText::new("no active sessions")
                    .color(theme().text.dim)
                    .small(),
            );
            return;
        }
        for s in &entry.sessions {
            let is_active = active == Some(&s.id);
            let mut btn = button::ghost(s.id.to_string());
            if is_active {
                btn = btn.variant(lutin_ui::widget::button::Variant::Primary);
            }
            if ui.add(btn).clicked() {
                intents.push(Intent::ActivateSession {
                    slug: slug.clone(),
                    session: s.id.clone(),
                });
            }
            let close = button::ghost("×");
            if ui.add(close).clicked() {
                intents.push(Intent::CloseSession {
                    slug: slug.clone(),
                    session: s.id.clone(),
                });
            }
            ui.add_space(4.0);
        }
    });
    intents
}

fn slug_error(e: &SlugError) -> String {
    format!("invalid slug: {e}")
}

fn display_name_error(e: &DisplayNameError) -> String {
    format!("invalid name: {e}")
}

/// Build a connection config from the active settings entry. Returns
/// `None` (and logs why) when settings are absent, the address can't
/// be turned into a valid URL, or the token is empty — the chrome
/// runs unconfigured so the user can fix it from the Settings view.
///
/// `addr` is taken verbatim into `ws://{addr}` so hostnames
/// (`localhost:7878`) work alongside numeric IPs. Url parsing rejects
/// anything that isn't a well-formed `host[:port]`.
fn build_cp_config(settings: &DesktopSettings) -> Option<CpConfig> {
    let active = settings.active()?;
    let addr = active.addr.trim();
    if addr.is_empty() {
        warn!(name = %active.name, "active connection has empty addr — running unconfigured");
        return None;
    }
    if active.token.trim().is_empty() {
        warn!(name = %active.name, "active connection has empty token — running unconfigured");
        return None;
    }
    let url = match url::Url::parse(&format!("ws://{addr}")) {
        Ok(u) => u,
        Err(e) => {
            warn!(name = %active.name, addr, error = %e, "could not build ws:// URL");
            return None;
        }
    };
    let token = Token::new(active.token.clone()).ok()?;
    Some(CpConfig { url, token })
}

