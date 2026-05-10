import { useEffect } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { cpSendOk, cpStatus, settingsGet, subscribeCp } from "./api";
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

function App() {
  const view = useApp((s) => s.view);
  const conn = useApp((s) => s.conn);
  const projects = useApp((s) => s.projects);
  const selected = useApp((s) => s.selectedProject);

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

  const activeProject = projects.find((p) => p.slug === selected) ?? null;

  return (
    <div className={styles.shell}>
      <TtsDownloadToast />
      <TopBar />
      <div className={styles.row}>
        <Sidebar />
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
              <div>No projects yet. Create one in the sidebar.</div>
            ) : (
              <div>Select a project.</div>
            )}
          </main>
        )}
      </div>
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
