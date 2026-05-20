import { useCallback, useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useAppKeybindDispatch } from "./appKeybinds";
import { cpSendOk, cpStatus, settingsGet, subscribeCp } from "./api";
import { QUICK_CHAT_WORKFLOW, useQuickChat } from "./quickChat";
import { CreateProjectModal } from "./components/CreateProjectModal";
import { LeaderHints } from "./components/LeaderHints";
import { ProjectPicker } from "./components/ProjectPicker";
import { SessionPicker } from "./components/SessionPicker";
import { WorkflowPicker } from "./components/WorkflowPicker";
import { Sidebar } from "./components/Sidebar";
import { SessionPane } from "./components/SessionPane";
import { SettingsView } from "./components/SettingsView";
import { TopBar } from "./components/TopBar";
import { TtsDownloadToast } from "./components/TtsDownloadToast";
import { useApp } from "./store";
import styles from "./App.module.css";

const ZOOM_KEY = "lutin.zoom";
const ZOOM_MIN = 0.6;
const ZOOM_MAX = 2.5;
const ZOOM_STEP = 0.1;

function applyZoom(z: number) {
  // Use the webview's native zoom factor instead of CSS `zoom`. CSS
  // `zoom` on Linux WebKitGTK pushes every layout/paint through a
  // software scaling path that wedges input handling — typing in a
  // textarea would freeze for seconds per character at non-1 scales.
  // The native path goes through `webkit_web_view_set_zoom_level`,
  // propagates to iframes for free, and is what the engine optimizes.
  getCurrentWebview()
    .setZoom(z)
    .catch(() => { /* non-fatal; persisted value still drives next run */ });
  localStorage.setItem(ZOOM_KEY, String(z));
}

function loadZoom(): number {
  const raw = localStorage.getItem(ZOOM_KEY);
  const n = raw ? Number(raw) : NaN;
  if (!Number.isFinite(n)) return 1;
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, n));
}

type Overlay = "project-picker" | "session-picker" | "new-session" | "create-project" | null;

function App() {
  const view = useApp((s) => s.view);
  const conn = useApp((s) => s.conn);
  const projects = useApp((s) => s.projects);
  const selected = useApp((s) => s.selectedProject);

  const [overlay, setOverlay] = useState<Overlay>(null);

  const openProjectPicker = useCallback(() => setOverlay("project-picker"), []);
  const openSessionPicker = useCallback(() => setOverlay("session-picker"), []);
  const openNewSessionPicker = useCallback(() => setOverlay("new-session"), []);
  const openQuickChat = useCallback(async () => {
    // Pinned quick-chat session lives across launches via localStorage.
    // Validation order: project still exists → session still exists in
    // that project's loaded list → reuse. Any miss creates a fresh
    // session and updates the pointer. Persona stays whatever the
    // workflow last selected — there's no per-session persona override
    // at create time, that's owned by the chat workflow's own UI.
    const state = useApp.getState();
    const quick = useQuickChat.getState();

    const fallbackProject =
      (quick.defaultProject &&
        state.projects.find((p) => p.slug === quick.defaultProject)?.slug) ||
      state.selectedProject ||
      state.projects[0]?.slug ||
      null;

    if (!fallbackProject) return; // nothing to do — no project anywhere

    const ptr = quick.sessionPtr;
    const reuseValid =
      ptr !== null &&
      state.projects.some((p) => p.slug === ptr.project) &&
      (state.sessionsBySlug[ptr.project] ?? []).some((s) => s.id === ptr.session);

    if (state.view.kind === "settings") state.setView({ kind: "project" });

    // After the session is selected/created, give the chat composer
    // keyboard focus so the user can start typing immediately. The
    // iframe hasn't mounted yet on first-create, so we delay one frame
    // and rely on the workflow's own shim handler to focus the first
    // textarea once it sees the message.
    const focusComposerSoon = () => {
      requestAnimationFrame(() => {
        window.dispatchEvent(new CustomEvent("lutin:focus-workflow"));
      });
    };

    if (reuseValid && ptr) {
      state.selectProject(ptr.project);
      state.selectSession(ptr.session);
      focusComposerSoon();
      return;
    }

    try {
      const r = await cpSendOk({
        StartSession: { slug: fallbackProject, workflow: QUICK_CHAT_WORKFLOW },
      });
      if (typeof r === "object" && "SessionStarted" in r) {
        const info = r.SessionStarted.info;
        state.applyEvent({ SessionStarted: { slug: fallbackProject, info } });
        state.selectProject(fallbackProject);
        state.selectSession(info.id);
        quick.setSessionPtr({ project: fallbackProject, session: info.id });
        focusComposerSoon();
      }
    } catch {
      /* Silently ignore — the user can retry, or open settings to
       * fix a missing default project. A toast surface would be nice
       * but isn't worth adding for this case. */
    }
  }, []);
  const focusWorkflow = useCallback(() => {
    // PluginIframe listens for this event and forwards focus to its
    // iframe; the workflow itself decides what to do with it (chat
    // focuses the composer).
    window.dispatchEvent(new CustomEvent("lutin:focus-workflow"));
  }, []);

  useAppKeybindDispatch({
    openProjectPicker,
    openSessionPicker,
    openNewSessionPicker,
    openQuickChat,
    focusWorkflow,
  });

  useEffect(() => {
    let zoom = loadZoom();
    applyZoom(zoom);
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      if (e.key === "=" || e.key === "+") {
        zoom = Math.min(ZOOM_MAX, +(zoom + ZOOM_STEP).toFixed(2));
      } else if (e.key === "-" || e.key === "_") {
        zoom = Math.max(ZOOM_MIN, +(zoom - ZOOM_STEP).toFixed(2));
      } else if (e.key === "0") {
        zoom = 1;
      } else {
        return;
      }
      e.preventDefault();
      applyZoom(zoom);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    const setConn = useApp.getState().setConn;
    const applyEvent = useApp.getState().applyEvent;
    const setSettings = useApp.getState().setSettings;

    let unlisten: (() => void) | null = null;
    (async () => {
      const settings = await settingsGet();
      setSettings(settings);

      unlisten = await subscribeCp({
        onConnected: () => setConn({ kind: "connected" }),
        onDisconnected: () => setConn({ kind: "disconnected" }),
        onHandshakeRejected: (reason) => setConn({ kind: "rejected", reason }),
        onConnectError: (error) => setConn({ kind: "error", error }),
        onCpEvent: (event) => applyEvent(event),
      });

      setConn(await cpStatus());
    })();

    return () => { if (unlisten) unlisten(); };
  }, []);

  useEffect(() => {
    if (conn.kind !== "connected") return;
    const setProjects = useApp.getState().setProjects;
    cpSendOk("ListProjects").then((r) => {
      if (typeof r === "object" && "Projects" in r) setProjects(r.Projects);
    });
  }, [conn.kind]);

  // Eagerly fetch sessions for every known project, on connect and
  // whenever a new project appears (e.g. via `ProjectCreated` event or
  // initial `ListProjects`). Without this the project picker shows an
  // empty preview for projects we haven't navigated into yet. We only
  // fetch when there's no slot in `sessionsBySlug` for the project so
  // re-renders don't spam the engine.
  useEffect(() => {
    if (conn.kind !== "connected") return;
    const setSessions = useApp.getState().setSessions;
    const have = useApp.getState().sessionsBySlug;
    for (const p of projects) {
      if (have[p.slug]) continue;
      cpSendOk({ ListSessions: { slug: p.slug } })
        .then((sr) => {
          if (typeof sr === "object" && "Sessions" in sr) {
            setSessions(p.slug, sr.Sessions);
          }
        })
        .catch(() => { /* picker just won't show last-use for this one */ });
    }
  }, [conn.kind, projects]);

  const activeProject = projects.find((p) => p.slug === selected) ?? null;

  return (
    <div className={styles.shell}>
      <TtsDownloadToast />
      <TopBar
        onOpenProjects={openProjectPicker}
        onCreateProject={() => setOverlay("create-project")}
      />
      <div className={styles.row}>
        {view.kind !== "settings" && <Sidebar />}
        {view.kind === "settings" ? (
          <SettingsView />
        ) : activeProject ? (
          <SessionPane project={activeProject} />
        ) : (
          <main className={styles.empty}>
            {conn.kind === "noconfig" ? (
              <NoConfig />
            ) : conn.kind === "connecting" ? (
              <div>Connecting to control panel…</div>
            ) : projects.length === 0 ? (
              <div>
                No projects yet.{" "}
                <button onClick={() => setOverlay("create-project")}>Create one</button>.
              </div>
            ) : (
              <div>
                Select a project ({" "}
                <button onClick={openProjectPicker}>open picker</button>{" "}).
              </div>
            )}
          </main>
        )}
      </div>

      {overlay === "project-picker" && (
        <ProjectPicker onClose={() => setOverlay(null)} />
      )}
      {overlay === "session-picker" && (
        <SessionPicker onClose={() => setOverlay(null)} />
      )}
      {overlay === "new-session" && (
        <WorkflowPicker onClose={() => setOverlay(null)} />
      )}
      {overlay === "create-project" && (
        <CreateProjectModal onClose={() => setOverlay(null)} />
      )}

      <LeaderHints />
    </div>
  );
}

function NoConfig() {
  const setView = useApp((s) => s.setView);
  return (
    <div className={styles.noconfig}>
      <div className={styles.noconfigTitle}>No control panel configured</div>
      <p>Add a control-panel connection to get started.</p>
      <button onClick={() => setView({ kind: "settings" })}>Open settings</button>
    </div>
  );
}

export default App;
