//! Desktop chrome — top-level egui app.
//!
//! Owns the four-slot layout (LeftSidebar / TopBar / RightSidebar /
//! Main) and the project picker. C2: when a project opens, dlopens
//! its workflow `.so`, builds a `Transport` paired to a tier-2 WS
//! bridge, and delegates the relevant slots to the workflow's
//! `WorkflowProjectUi`. C3: real `ChromeApi`, session lifecycle (mint
//! `WorkflowSessionUi`s for each active session, render via Main slot
//! with a chrome-owned tab strip), workflow build progress.
//!
//! All per-opened-project state lives in `App::projects_state` keyed
//! by `Slug`. One entry owns the chrome's tier-2 worker, the loaded
//! project UI (if any), the session list, every loaded session UI,
//! the focused session id, and the latest build status. Dropping the
//! entry tears the lot down in the right order via Drop.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Align, CentralPanel, Color32, Layout, RichText};
use lutin_control_protocol::{
    DisplayName, DisplayNameError, Event as CpEvent, ProjectEndpoint as CpProjectEndpoint,
    ProjectInfo, ProjectStatus, Request, Response, ResponseOk, SessionEndpoint as CpSessionEndpoint,
    SessionInfo, Slug, WorkflowInfo,
};
use lutin_ids::{SessionId, SlugError, WorkflowId};
use lutin_ui::prelude::*;
use lutin_ui::widget::{button, panel, text_input};
use lutin_workflow_ui::{
    AuthToken, ChromeApi, ProjectCtx, ProjectEndpoint as UiProjectEndpoint,
    SessionCtx, SessionEndpoint as UiSessionEndpoint, Slot, Transport, WorkflowProjectUi,
    WorkflowSessionUi,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::bridge::{self, make_transport_pair};
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
    StopProject(Slug),
    DeleteProject(Slug),
    SubmitCreate {
        slug: Slug,
        display_name: DisplayName,
    },
    EditNewSlug(String),
    EditNewDisplay(String),
    SetFormError(String),
    ClearFormError,
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

struct LoadedProject {
    ui: Box<dyn WorkflowProjectUi>,
    /// Aborts the workflow's WS bridge task on drop.
    _bridge: BridgeGuard,
    _lib: Arc<WorkflowLibrary>,
    manifest: lutin_workflow_ui::Manifest,
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

/// Everything chrome owns about one opened project. Dropping the entry
/// tears the whole subtree down: workflow UI → workflow bridge,
/// session UIs → session bridges.
struct ProjectEntry {
    endpoint: CpProjectEndpoint,
    /// `None` when the workflow `.so` failed to dlopen.
    loaded: Option<LoadedProject>,
    /// Sessions reported by CP (mirrors `ListSessions` +
    /// `SessionStarted`/`SessionEnded` broadcasts).
    sessions: Vec<SessionInfo>,
    /// Loaded session UIs keyed by session id.
    loaded_sessions: HashMap<SessionId, LoadedSession>,
    /// Currently focused session in this project's tab strip.
    active_session: Option<SessionId>,
}

pub enum ChromeIntent {
    StartSession {
        project: Slug,
        workflow: WorkflowId,
    },
    ActivateSession {
        project: Slug,
        session: SessionId,
    },
    Notify(String),
}

/// Real `ChromeApi` impl handed to every `WorkflowProjectUi` /
/// `WorkflowSessionUi`. Calls are non-blocking — they push onto an mpsc
/// the App drains during its frame loop. Cheaply cloneable (it's just a
/// sender) so the App hands a fresh clone to each render context.
#[derive(Clone)]
struct RealChromeApi {
    tx: mpsc::UnboundedSender<ChromeIntent>,
}

impl ChromeApi for RealChromeApi {
    fn start_session(&self, project: &Slug, workflow: &WorkflowId) {
        let _ = self.tx.send(ChromeIntent::StartSession {
            project: project.clone(),
            workflow: workflow.clone(),
        });
    }
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

    pending_opens: HashMap<RequestId, Slug>,
    /// Slug a `ListSessions` reply is for.
    pending_list_sessions: HashMap<RequestId, Slug>,
    /// `(slug, session)` an `OpenSession` reply is for.
    pending_session_opens: HashMap<RequestId, (Slug, SessionId)>,
    /// Slug a `StartSession` reply is for.
    pending_starts: HashMap<RequestId, Slug>,

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
            pending_opens: HashMap::new(),
            pending_list_sessions: HashMap::new(),
            pending_session_opens: HashMap::new(),
            pending_starts: HashMap::new(),
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
        self.pending_opens.clear();
        self.pending_list_sessions.clear();
        self.pending_session_opens.clear();
        self.pending_starts.clear();
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

    /// Attempt to dlopen the workflow `.so` and install a fresh
    /// `ProjectEntry`. Always inserts an entry — even when the
    /// workflow UI failed to load, chrome still tracks the project.
    fn load_workflow_for(&mut self, slug: Slug, endpoint: CpProjectEndpoint) {
        let workflow_id = default_workflow_id();
        let loaded = match self.workflow_cache.load(&slug, &workflow_id) {
            Ok(lib) => {
                let manifest = lib.workflow().manifest();
                // Transitional: workflow ProjectUi gets a dummy transport
                // in Phase 4.3. The legacy project-tier protocol it used
                // to send over this is dead; the trait itself is being
                // removed in Phase 6/cleanup.
                let transport = dummy_transport(&self.tokio);
                // Phase 4.3: no per-project WS exists. addr/token are
                // placeholders; the chat workflow's ProjectUi doesn't
                // dial them anymore.
                let ui_endpoint = UiProjectEndpoint {
                    slug: slug.clone(),
                    workflow: workflow_id.clone(),
                    addr: "127.0.0.1:0".parse().unwrap(),
                    token: AuthToken::new(String::new()),
                };
                // No bridge task to spawn — install a no-op guard so
                // the field's type stays uniform.
                let no_op = self.tokio.spawn(async {});
                let ui = lib.workflow().open_project(ui_endpoint, transport);
                Some(LoadedProject {
                    ui,
                    _bridge: BridgeGuard(no_op.abort_handle()),
                    _lib: lib,
                    manifest,
                })
            }
            Err(e) => {
                self.last_error = Some(format!("workflow load failed: {e}"));
                None
            }
        };

        self.projects_state.insert(
            slug.clone(),
            ProjectEntry {
                endpoint,
                loaded,
                sessions: Vec::new(),
                loaded_sessions: HashMap::new(),
                active_session: None,
            },
        );
        // Initial session-list refresh; broadcasts keep it fresh after.
        if let Ok(id) = self.send(Request::ListSessions { slug: slug.clone() }) {
            self.pending_list_sessions.insert(id, slug);
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
                ChromeIntent::StartSession { project, workflow } => {
                    match self.send(Request::StartSession {
                        slug: project.clone(),
                        workflow,
                    }) {
                        Ok(id) => {
                            self.pending_starts.insert(id, project);
                        }
                        Err(e) => {
                            self.last_error = Some(e.to_string());
                        }
                    }
                }
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

    fn on_response(&mut self, request_id: RequestId, resp: Response) {
        let pending_open = self.pending_opens.remove(&request_id);
        let pending_list = self.pending_list_sessions.remove(&request_id);
        let pending_session_open = self.pending_session_opens.remove(&request_id);
        let pending_start = self.pending_starts.remove(&request_id);
        match resp {
            Response::Ok(ok) => match ok {
                ResponseOk::Projects(list) => {
                    self.projects = list;
                    let live_slugs: std::collections::HashSet<Slug> = self
                        .projects
                        .iter()
                        .filter(|p| {
                            !matches!(p.status, ProjectStatus::Stopped | ProjectStatus::Failed)
                        })
                        .map(|p| p.slug.clone())
                        .collect();
                    if let Some(slug) = &self.active
                        && !self.projects.iter().any(|p| &p.slug == slug)
                    {
                        self.active = None;
                    }
                    self.projects_state
                        .retain(|slug, _| live_slugs.contains(slug));
                    self.pending_opens
                        .retain(|_, slug| self.projects.iter().any(|p| &p.slug == slug));
                    self.pending_list_sessions
                        .retain(|_, slug| live_slugs.contains(slug));
                    self.pending_session_opens
                        .retain(|_, (slug, _)| live_slugs.contains(slug));
                    self.pending_starts
                        .retain(|_, slug| live_slugs.contains(slug));
                }
                ResponseOk::Created(_) => {
                    self.new_slug.clear();
                    self.new_display.clear();
                    self.new_error = None;
                }
                ResponseOk::Deleted => {}
                ResponseOk::Opened(endpoint) => {
                    if let Some(slug) = pending_open {
                        self.load_workflow_for(slug, endpoint);
                    }
                }
                ResponseOk::Stopped => {}
                ResponseOk::Workflows(list) => {
                    self.workflows = list;
                }
                ResponseOk::Sessions(list) => {
                    let Some(slug) = pending_list else {
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
                    let Some(slug) = pending_start else {
                        warn!("SessionStarted response without pending StartSession");
                        return;
                    };
                    let session_id = info.id.clone();
                    self.upsert_session(&slug, info);
                    self.load_session(slug, session_id, endpoint);
                }
                ResponseOk::SessionStopped => {}
                ResponseOk::SessionOpened(endpoint) => {
                    let Some((slug, session)) = pending_session_open else {
                        warn!("SessionOpened response without pending OpenSession");
                        return;
                    };
                    self.load_session(slug, session, endpoint);
                }
            },
            Response::Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn on_broadcast(&mut self, ev: CpEvent) {
        match ev {
            CpEvent::ProjectCreated(_)
            | CpEvent::ProjectDeleted { .. }
            | CpEvent::ProjectStatusChanged { .. } => {
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

    #[allow(dead_code)]
    fn request_open_session(&mut self, slug: &Slug, session: SessionId) {
        if self
            .entry(slug)
            .is_some_and(|e| e.loaded_sessions.contains_key(&session))
        {
            return;
        }
        let id = match self.send(Request::OpenSession {
            slug: slug.clone(),
            session: session.clone(),
        }) {
            Ok(id) => id,
            Err(_) => return,
        };
        self.pending_session_opens
            .insert(id, (slug.clone(), session));
    }

    fn load_session(
        &mut self,
        slug: Slug,
        session: SessionId,
        ep: CpSessionEndpoint,
    ) {
        let workflow_id = default_workflow_id();
        let lib = match self.workflow_cache.load(&slug, &workflow_id) {
            Ok(lib) => lib,
            Err(e) => {
                self.last_error = Some(format!("workflow load failed: {e}"));
                return;
            }
        };

        let (transport, bridge_endpoints) = make_transport_pair();
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
                    match self.send(Request::OpenProject { slug: slug.clone() }) {
                        Ok(id) => {
                            self.pending_opens.insert(id, slug);
                        }
                        Err(e) => {
                            self.last_error = Some(e.to_string());
                        }
                    }
                }
                Intent::StopProject(slug) => {
                    if let Err(e) = self.send(Request::StopProject { slug }) {
                        self.last_error = Some(e.to_string());
                    }
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

        let active_wants_right = self
            .active
            .clone()
            .and_then(|s| {
                self.entry(&s)
                    .and_then(|e| e.loaded.as_ref().map(|lp| lp.manifest.wants_right_sidebar))
            })
            .unwrap_or(false);
        if active_wants_right {
            egui::Panel::right("chrome-right")
                .resizable(true)
                .default_size(260.0)
                .min_size(200.0)
                .show_inside(ui, |ui| {
                    self.render_workflow_slot(Slot::RightSidebar, ui);
                });
        } else {
            egui::Panel::right("chrome-right")
                .resizable(true)
                .default_size(260.0)
                .min_size(200.0)
                .show_inside(ui, |ui| {
                    intents.extend(draw_right_sidebar(self, ui));
                });
        }

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
    fn render_workflow_slot(&mut self, slot: Slot, ui: &mut egui::Ui) {
        let Some(slug) = self.active.clone() else {
            return;
        };
        let chrome = self.chrome_api.clone();
        let Some(entry) = self.entry_mut(&slug) else {
            return;
        };
        let active_session = entry.active_session.clone();
        let Some(loaded) = entry.loaded.as_mut() else {
            return;
        };
        let ctx = ProjectCtx {
            chrome: &chrome,
            slug: &slug,
            active_session: active_session.as_ref(),
        };
        loaded.ui.render(slot, ctx, ui);
    }

    /// Render the active project's session UI (if any) into the Main
    /// slot. Returns true iff a session UI rendered.
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
        loaded.ui.render(Slot::Main, ctx, ui);
        true
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
        app.render_workflow_slot(Slot::TopBar, ui);
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
                        let label =
                            format!("{}  ·  {}", p.display_name.as_str(), status_str(&p.status));
                        let mut btn = button::ghost(label).full_width();
                        if is_active {
                            btn = btn.variant(lutin_ui::widget::button::Variant::Primary);
                        }
                        if ui.add(btn).clicked() {
                            intents.push(Intent::SelectProject(p.slug.clone()));
                            let need_open = !matches!(p.status, ProjectStatus::Running)
                                || app.entry(&p.slug).is_none();
                            if need_open {
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
    app.render_workflow_slot(Slot::LeftSidebar, ui);
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
        ui.label(
            RichText::new(format!("status: {}", status_str(&p.status)))
                .color(theme().text.dim)
                .small(),
        );
        ui.add_space(8.0);
        if let Some(entry) = app.entry(slug) {
            ui.label(
                RichText::new(format!("endpoint: {}", entry.endpoint.addr))
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

    let has_loaded = app
        .entry(&slug)
        .is_some_and(|e| e.loaded.is_some());
    if has_loaded {
        app.render_workflow_slot(Slot::Main, ui);
        return intents;
    }

    let info = app.projects.iter().find(|p| p.slug == slug).cloned();
    let endpoint_addr = app.entry(&slug).map(|e| e.endpoint.addr);
    ui.vertical(|ui| {
        if let Some(p) = &info {
            ui.label(RichText::new(p.display_name.as_str()).size(18.0).strong());
            ui.label(
                RichText::new(format!("status: {}", status_str(&p.status)))
                    .color(theme().text.dim)
                    .small(),
            );
            ui.add_space(12.0);
        }
        match endpoint_addr {
            None => {
                ui.label(RichText::new("project not yet opened").color(theme().text.dim));
            }
            Some(addr) => {
                ui.label(
                    RichText::new("project running — workflow UI not loaded")
                        .color(theme().text.dim),
                );
                ui.label(
                    RichText::new(format!("listening on {addr}"))
                        .color(theme().text.dim)
                        .small(),
                );
            }
        }
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let running = info
                .as_ref()
                .map(|p| matches!(p.status, ProjectStatus::Running))
                .unwrap_or(false);
            let mut stop = button::secondary("Stop");
            if !running {
                stop = stop.disabled();
            }
            if ui.add(stop).clicked()
                && let Some(p) = &info
            {
                intents.push(Intent::StopProject(p.slug.clone()));
            }
            let mut delete = button::danger("Delete");
            if running {
                delete = delete.disabled();
            }
            if ui.add(delete).clicked()
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

/// Phase 4.3 transitional dummy: workflow ProjectUi receives a
/// `Transport` whose channels go nowhere. Sends are silently consumed
/// by a parked receiver; reads return `None` immediately because the
/// matching sender is dropped on the spot. The trait itself is being
/// removed in Phase 6/cleanup.
fn dummy_transport(tokio: &tokio::runtime::Handle) -> Transport {
    let (send, mut send_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    // Park the receiver in a never-completing task so sends don't fail
    // synchronously; the task just drains and drops.
    tokio.spawn(async move { while send_rx.recv().await.is_some() {} });
    let (_recv_tx, recv) = mpsc::unbounded_channel::<Vec<u8>>();
    Transport { send, recv }
}

fn status_str(s: &ProjectStatus) -> &'static str {
    match s {
        ProjectStatus::Stopped => "stopped",
        ProjectStatus::Starting => "starting",
        ProjectStatus::Running => "running",
        ProjectStatus::Stopping => "stopping",
        ProjectStatus::Failed => "failed",
    }
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

