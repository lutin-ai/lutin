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
  const applyEvent = useApp((s) => s.applyEvent);

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
          setWorkflows(dedupById(r.Workflows));
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
        setWorkflows(dedupById(r.Workflows));
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
      const r = await cpSendOk({ StartSession: { slug: project.slug, workflow } });
      // Seed the new session into the store from the response and
      // select it immediately. The CP also broadcasts SessionStarted
      // around the same time; applyEvent is idempotent on session id
      // so racing the two is harmless. Doing it here avoids the brief
      // "no session selected" flash where the body falls back to the
      // previous session before the broadcast lands.
      if (typeof r === "object" && "SessionStarted" in r) {
        const info = r.SessionStarted.info;
        applyEvent({ SessionStarted: { slug: project.slug, info } });
        select(info.id);
      }
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

  const deleteSession = async (id: string) => {
    setError(null);
    try {
      await cpSendOk({ DeleteSession: { slug: project.slug, session: id } });
      // SessionEnded event removes the row from the store.
    } catch (e) {
      setError(String(e));
    }
  };

  const refreshSessions = async () => {
    try {
      const r = await cpSendOk({ ListSessions: { slug: project.slug } });
      if (typeof r === "object" && "Sessions" in r) {
        setSessions(project.slug, r.Sessions);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const activeSession = sessions.find((s) => s.id === selected) ?? null;

  // Sort by last_activity desc (running and dormant interleaved by
  // recency), falling back to created_at when summary hasn't been
  // written yet. This matches the user's mental model of "most
  // recently touched session at the top."
  const sortedSessions = [...sessions].sort((a, b) => {
    const ka = a.summary?.last_activity ?? a.created_at;
    const kb = b.summary?.last_activity ?? b.created_at;
    return kb.localeCompare(ka);
  });

  return (
    <main className={styles.pane}>
      <header className={styles.header}>
        <div className={styles.crumbs}>
          <span>{project.slug}</span>
          <span className={styles.crumbSep}>/</span>
          <span>sessions</span>
        </div>
        <h1 className={styles.title}>{project.display_name}</h1>
      </header>

      <section className={styles.tabs}>
        {loadingSessions && <span className={styles.loading}>Loading sessions…</span>}
        {sortedSessions.map((s) => {
          const label = s.summary?.title?.trim() || `${s.workflow} · ${shortId(s.id)}`;
          const sublabel = s.summary?.subtitle?.trim()
            || (s.summary?.last_activity ?? s.created_at).slice(0, 16).replace("T", " ");
          return (
            <div
              key={s.id}
              className={`${styles.tab} ${selected === s.id ? styles.tabActive : ""}`}
              onClick={() => select(s.id)}
              title={s.summary?.preview ?? undefined}
            >
              <span className={styles.tabLabel}>
                <span
                  className={styles.tabState}
                  data-state={s.state.toLowerCase()}
                  title={s.state === "Running" ? "Running" : "Dormant — click to resume"}
                />
                {label}
              </span>
              <span className={styles.tabId}>{sublabel}</span>
              {s.state === "Running" && (
                <button
                  className={styles.tabClose}
                  title="Stop session (keep history)"
                  onClick={(e) => {
                    e.stopPropagation();
                    stopSession(s.id);
                  }}
                  aria-label="Stop session"
                >
                  <IconStop />
                </button>
              )}
              <button
                className={styles.tabDelete}
                title="Delete session permanently"
                onClick={(e) => {
                  e.stopPropagation();
                  if (confirm(`Delete session "${label}" permanently? History will be lost.`)) {
                    deleteSession(s.id);
                  }
                }}
                aria-label="Delete session"
              >
                <IconClose />
              </button>
            </div>
          );
        })}
        <button className={styles.newSession} onClick={loadWorkflows}>
          <IconPlus /> New session
        </button>
        <button
          className={styles.refresh}
          onClick={refreshSessions}
          title="Refresh"
          aria-label="Refresh"
        >
          <IconRefresh />
        </button>
      </section>

      {picking && workflows && (
        <div className={styles.workflowPicker}>
          <div className={styles.workflowPickerHeader}>
            Pick a workflow
            <button onClick={() => setPicking(false)} aria-label="Close picker">
              <IconClose />
            </button>
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
              <span className={styles.workflowIcon}>
                {w.icon ? w.icon : <IconWorkflow />}
              </span>
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
                <div className={styles.placeholderIcon}>
                  <IconHourglass />
                </div>
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

function IconStop() {
  return (
    <svg width="9" height="9" viewBox="0 0 10 10" fill="currentColor" aria-hidden>
      <rect x="2" y="2" width="6" height="6" rx="0.8" />
    </svg>
  );
}
function IconClose() {
  return (
    <svg width="11" height="11" viewBox="0 0 12 12" fill="none" aria-hidden>
      <path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}
function IconRefresh() {
  return (
    <svg width="13" height="13" viewBox="0 0 14 14" fill="none" aria-hidden>
      <path
        d="M12 7a5 5 0 1 1-1.5-3.5M12 2v3.5h-3.5"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
function IconPlus() {
  return (
    <svg width="11" height="11" viewBox="0 0 12 12" fill="none" aria-hidden>
      <path d="M6 2v8M2 6h8" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}
function IconWorkflow() {
  return (
    <svg width="16" height="16" viewBox="0 0 18 18" fill="none" aria-hidden>
      <rect x="2.5" y="2.5" width="13" height="13" rx="2.5" stroke="currentColor" strokeWidth="1.3" />
      <path d="M5.5 9h7M5.5 6h4M5.5 12h5" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
    </svg>
  );
}
function IconHourglass() {
  return (
    <svg width="36" height="36" viewBox="0 0 36 36" fill="none" aria-hidden>
      <path
        d="M11 5h14M11 31h14M12 5v5l6 5 6-5V5M12 31v-5l6-5 6 5v5"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// CP's `ListWorkflows` returns one entry per installed image, so two
// chat images with different tags both come back as `{ id: "chat" }`.
// Keep the first (most-recent ordering on the CP side) and drop the
// rest so React keys stay unique. Real fix is CP-side: collapse by id
// and prefer the newest digest.
function dedupById<T extends { id: string }>(xs: T[]): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const x of xs) {
    if (seen.has(x.id)) continue;
    seen.add(x.id);
    out.push(x);
  }
  return out;
}
