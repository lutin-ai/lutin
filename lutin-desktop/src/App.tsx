import { useEffect } from "react";
import { cpSendOk, cpStatus, settingsGet, subscribeCp } from "./api";
import { Sidebar } from "./components/Sidebar";
import { SessionPane } from "./components/SessionPane";
import { SettingsView } from "./components/SettingsView";
import { TopBar } from "./components/TopBar";
import { useApp } from "./store";
import styles from "./App.module.css";

function App() {
  const view = useApp((s) => s.view);
  const conn = useApp((s) => s.conn);
  const projects = useApp((s) => s.projects);
  const selected = useApp((s) => s.selectedProject);

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
