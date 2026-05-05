import { useEffect, useState } from "react";
import { cpSendOk } from "../api";
import { useApp } from "../store";
import type { ProjectInfo, WorkflowInfo } from "../types";
import { PluginIframe } from "./PluginIframe";
import styles from "./SessionPane.module.css";

interface Props { project: ProjectInfo }

const EMPTY: never[] = [];

export function SessionPane({ project }: Props) {
  // Selector returns the stored slot directly; defaulting `?? []`
  // inside the selector would allocate a fresh array every render
  // and trip Zustand's snapshot equality check.
  const sessions = useApp((s) => s.sessionsBySlug[project.slug]) ?? EMPTY;
  const setSessions = useApp((s) => s.setSessions);
  const selected = useApp((s) => s.selectedSession);
  const select = useApp((s) => s.selectSession);

  const [workflows, setWorkflows] = useState<WorkflowInfo[] | null>(null);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);

  // Refresh sessions on project change.
  useEffect(() => {
    let cancelled = false;
    setLoadingSessions(true);
    setError(null);
    cpSendOk({ ListSessions: { slug: project.slug } })
      .then((r) => {
        if (cancelled) return;
        if (typeof r === "object" && "Sessions" in r) {
          setSessions(project.slug, r.Sessions);
        }
      })
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoadingSessions(false));
    return () => { cancelled = true; };
  }, [project.slug, setSessions]);

  // Always-on workflow listing so we can resolve digest → bundle for
  // any active session, not just those started in the current picker
  // session. Re-fetched on project change since the install set is
  // global today but may turn project-scoped later.
  useEffect(() => {
    let cancelled = false;
    cpSendOk("ListWorkflows")
      .then((r) => {
        if (cancelled) return;
        if (typeof r === "object" && "Workflows" in r) {
          setWorkflows(r.Workflows);
        }
      })
      .catch(() => { /* surfaced by the picker if the user opens it */ });
    return () => { cancelled = true; };
  }, [project.slug]);

  const loadWorkflows = async () => {
    setError(null);
    try {
      const r = await cpSendOk("ListWorkflows");
      if (typeof r === "object" && "Workflows" in r) {
        setWorkflows(r.Workflows);
        setPicking(true);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const startSession = async (workflow: string) => {
    setError(null);
    setPicking(false);
    try {
      await cpSendOk({ StartSession: { slug: project.slug, workflow } });
      // SessionStarted event will populate the list via the store.
    } catch (e) {
      setError(String(e));
    }
  };

  const stopSession = async (id: string) => {
    setError(null);
    try {
      await cpSendOk({ StopSession: { slug: project.slug, session: id } });
    } catch (e) {
      setError(String(e));
    }
  };

  const activeSession = sessions.find((s) => s.id === selected) ?? null;

  return (
    <main className={styles.pane}>
      <header className={styles.header}>
        <h1 className={styles.title}>{project.display_name}</h1>
        <span className={styles.slug}>{project.slug}</span>
      </header>

      <section className={styles.tabs}>
        {loadingSessions && <span className={styles.loading}>Loading sessions…</span>}
        {sessions.map((s) => (
          <div
            key={s.id}
            className={`${styles.tab} ${selected === s.id ? styles.tabActive : ""}`}
            onClick={() => select(s.id)}
          >
            <span className={styles.tabLabel}>{s.workflow}</span>
            <span className={styles.tabId}>{shortId(s.id)}</span>
            <button
              className={styles.tabClose}
              title="Stop session"
              onClick={(e) => {
                e.stopPropagation();
                if (confirm(`Stop session ${shortId(s.id)}?`)) stopSession(s.id);
              }}
            >
              ×
            </button>
          </div>
        ))}
        <button className={styles.newSession} onClick={loadWorkflows}>
          + New session
        </button>
      </section>

      {picking && workflows && (
        <div className={styles.workflowPicker}>
          <div className={styles.workflowPickerHeader}>
            Pick a workflow
            <button onClick={() => setPicking(false)}>×</button>
          </div>
          {workflows.length === 0 && (
            <div className={styles.empty}>No workflows installed on the control panel.</div>
          )}
          {workflows.map((w) => (
            <button
              key={w.id}
              className={styles.workflowOption}
              onClick={() => startSession(w.id)}
            >
              <span className={styles.workflowIcon}>{w.icon || "▮"}</span>
              <span>
                <div>{w.display_name}</div>
                <div className={styles.workflowId}>{w.id}</div>
              </span>
            </button>
          ))}
        </div>
      )}

      {error && <div className={styles.error}>{error}</div>}

      <section className={styles.body}>
        {activeSession ? (() => {
          const wf = workflows?.find((w) => w.id === activeSession.workflow);
          if (!wf) {
            return (
              <div className={styles.placeholder}>
                <div className={styles.placeholderIcon}>⏳</div>
                <div className={styles.placeholderTitle}>Resolving workflow…</div>
                <div className={styles.placeholderSub}>
                  Waiting for workflow metadata for <code>{activeSession.workflow}</code>.
                </div>
              </div>
            );
          }
          return (
            <PluginIframe
              key={activeSession.id}
              slug={project.slug}
              session={activeSession.id}
              workflow={activeSession.workflow}
              digest={wf.digest}
            />
          );
        })() : (
          <div className={styles.empty}>
            {sessions.length === 0
              ? "No sessions yet. Start one above."
              : "Select a session."}
          </div>
        )}
      </section>
    </main>
  );
}

function shortId(id: string): string {
  return id.length > 10 ? id.slice(0, 8) + "…" : id;
}
