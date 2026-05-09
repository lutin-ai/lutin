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

  const [workflows, setWorkflows] = useState<WorkflowInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Keep the session list warm. The Sidebar also lists sessions, so
  // this is technically redundant when the sidebar is mounted; the
  // store de-dupes by id so the duplicate fetch is harmless and lets
  // SessionPane stand alone if the sidebar is ever hidden.
  useEffect(() => {
    let cancelled = false;
    setError(null);
    cpSendOk({ ListSessions: { slug: project.slug } })
      .then((r) => {
        if (cancelled) return;
        if (typeof r === "object" && "Sessions" in r) {
          setSessions(project.slug, r.Sessions);
        }
      })
      .catch((e) => !cancelled && setError(String(e)));
    return () => { cancelled = true; };
  }, [project.slug, setSessions]);

  // Workflow list is needed to resolve digest → bundle for the
  // active session's iframe.
  useEffect(() => {
    let cancelled = false;
    cpSendOk("ListWorkflows")
      .then((r) => {
        if (cancelled) return;
        if (typeof r === "object" && "Workflows" in r) {
          setWorkflows(dedupById(r.Workflows));
        }
      })
      .catch(() => { /* iframe will sit on the resolving placeholder */ });
    return () => { cancelled = true; };
  }, [project.slug]);

  const activeSession = sessions.find((s) => s.id === selected) ?? null;
  const sessionTitle = activeSession?.summary?.title?.trim() || null;
  const headerTitle = sessionTitle ?? project.display_name;

  return (
    <main className={styles.pane}>
      <header className={styles.header}>
        <div className={styles.crumbs}>
          <span>{project.slug}</span>
          <span className={styles.crumbSep}>/</span>
          <span>chats</span>
        </div>
        <h1 className={styles.title}>{headerTitle}</h1>
      </header>

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
              ? "No chats yet. Start one from the sidebar."
              : "Select a chat from the sidebar."}
          </div>
        )}
      </section>
    </main>
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
