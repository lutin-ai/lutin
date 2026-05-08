import { useEffect, useRef, useState } from "react";
import { cpSendOk } from "../api";
import { useApp } from "../store";
import type { SessionInfo, WorkflowInfo } from "../types";
import styles from "./Sidebar.module.css";

const CHAT_WORKFLOW_ID = "chat";
const SIDEBAR_WIDTH_KEY = "lutin.sidebar.width";
const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 600;
const SIDEBAR_DEFAULT = 240;

function loadInitialWidth(): number {
  const raw = localStorage.getItem(SIDEBAR_WIDTH_KEY);
  const n = raw ? Number(raw) : NaN;
  if (!Number.isFinite(n)) return SIDEBAR_DEFAULT;
  return Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, n));
}

export function Sidebar() {
  const projects = useApp((s) => s.projects);
  const selected = useApp((s) => s.selectedProject);
  const select = useApp((s) => s.selectProject);
  const view = useApp((s) => s.view);
  const setView = useApp((s) => s.setView);
  const conn = useApp((s) => s.conn);
  const sessionsBySlug = useApp((s) => s.sessionsBySlug);
  const setSessions = useApp((s) => s.setSessions);
  const selectedSession = useApp((s) => s.selectedSession);
  const selectSession = useApp((s) => s.selectSession);
  const applyEvent = useApp((s) => s.applyEvent);

  const [width, setWidth] = useState<number>(() => loadInitialWidth());
  const [dragging, setDragging] = useState(false);
  const dragStartRef = useRef<{ x: number; w: number } | null>(null);

  useEffect(() => {
    if (!dragging) return;
    const onMove = (e: MouseEvent) => {
      const start = dragStartRef.current;
      if (!start) return;
      const next = Math.min(
        SIDEBAR_MAX,
        Math.max(SIDEBAR_MIN, start.w + (e.clientX - start.x)),
      );
      setWidth(next);
    };
    const onUp = () => {
      setDragging(false);
      dragStartRef.current = null;
    };
    const prevCursor = document.body.style.cursor;
    const prevSelect = document.body.style.userSelect;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.cursor = prevCursor;
      document.body.style.userSelect = prevSelect;
    };
  }, [dragging]);

  useEffect(() => {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
  }, [width]);

  const startResize = (e: React.MouseEvent) => {
    e.preventDefault();
    dragStartRef.current = { x: e.clientX, w: width };
    setDragging(true);
  };

  const [creating, setCreating] = useState(false);
  const [newSlug, setNewSlug] = useState("");
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [chatWorkflow, setChatWorkflow] = useState<WorkflowInfo | null>(null);

  // Resolve the chat workflow once on connect so the "+" button can
  // start a session without a picker. If the chat image isn't
  // installed, we hide the "+" rather than show an error.
  useEffect(() => {
    if (conn.kind !== "connected") return;
    let cancelled = false;
    cpSendOk("ListWorkflows")
      .then((r) => {
        if (cancelled) return;
        if (typeof r === "object" && "Workflows" in r) {
          const found = r.Workflows.find((w) => w.id === CHAT_WORKFLOW_ID);
          setChatWorkflow(found ?? null);
        }
      })
      .catch(() => { /* picker stays hidden */ });
    return () => { cancelled = true; };
  }, [conn.kind]);

  const submit = async () => {
    if (!newSlug.trim() || !newName.trim()) return;
    setError(null);
    try {
      await cpSendOk({
        CreateProject: { slug: newSlug.trim(), display_name: newName.trim() },
      });
      setNewSlug("");
      setNewName("");
      setCreating(false);
    } catch (e) {
      setError(String(e));
    }
  };

  const remove = async (slug: string) => {
    setError(null);
    try {
      await cpSendOk({ DeleteProject: { slug } });
    } catch (e) {
      setError(String(e));
    }
  };

  const onSelect = (slug: string) => {
    if (view.kind === "settings") setView({ kind: "project" });
    select(slug);
  };

  const inSettings = view.kind === "settings";

  // Chats list for the active project, filtered to chat-workflow
  // sessions and sorted most-recently-active first.
  const chats: SessionInfo[] = (() => {
    if (!selected || inSettings) return [];
    const all = sessionsBySlug[selected] ?? [];
    return [...all]
      .filter((s) => s.workflow === CHAT_WORKFLOW_ID)
      .sort((a, b) => {
        const ka = a.summary?.last_activity ?? a.created_at;
        const kb = b.summary?.last_activity ?? b.created_at;
        return kb.localeCompare(ka);
      });
  })();

  const startChat = async () => {
    if (!selected || !chatWorkflow) return;
    setError(null);
    try {
      const r = await cpSendOk({
        StartSession: { slug: selected, workflow: chatWorkflow.id },
      });
      if (typeof r === "object" && "SessionStarted" in r) {
        const info = r.SessionStarted.info;
        applyEvent({ SessionStarted: { slug: selected, info } });
        selectSession(info.id);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const deleteChat = async (id: string) => {
    if (!selected) return;
    try {
      await cpSendOk({ DeleteSession: { slug: selected, session: id } });
    } catch (e) {
      setError(String(e));
    }
  };

  // Refresh chats when the project changes. The store already keeps
  // sessionsBySlug warm via SessionStarted/Ended events, but a
  // freshly-selected project needs an initial fill.
  useEffect(() => {
    if (!selected || inSettings) return;
    cpSendOk({ ListSessions: { slug: selected } })
      .then((r) => {
        if (typeof r === "object" && "Sessions" in r) {
          setSessions(selected, r.Sessions);
        }
      })
      .catch(() => { /* SessionPane will retry */ });
  }, [selected, inSettings, setSessions]);

  return (
    <aside className={styles.sidebar} style={{ width }}>
      <div
        className={`${styles.resizer} ${dragging ? styles.dragging : ""}`}
        onMouseDown={startResize}
        role="separator"
        aria-orientation="vertical"
      />
      <div className={styles.sectionHead}>
        <span className={styles.sectionLabel}>Projects</span>
        <button
          className={styles.addBtn}
          title="New project"
          disabled={conn.kind !== "connected" || creating}
          onClick={() => setCreating(true)}
        >
          +
        </button>
      </div>

      <ul className={styles.list}>
        {projects.map((p) => (
          <li
            key={p.slug}
            className={!inSettings && selected === p.slug ? styles.active : undefined}
            onClick={() => onSelect(p.slug)}
          >
            <span className={styles.folderIcon} aria-hidden>
              <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4">
                <path d="M1.5 4.5a1 1 0 0 1 1-1h3.2a1 1 0 0 1 .7.3l1 1h6.1a1 1 0 0 1 1 1v6.7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z" />
              </svg>
            </span>
            <span className={styles.projName}>{p.display_name}</span>
            <button
              className={styles.deleteBtn}
              title="Delete"
              onClick={(e) => {
                e.stopPropagation();
                if (confirm(`Delete project ${p.slug}?`)) remove(p.slug);
              }}
            >
              ×
            </button>
          </li>
        ))}
      </ul>

      {creating && (
        <div className={styles.creator}>
          <input
            placeholder="slug"
            value={newSlug}
            onChange={(e) => setNewSlug(e.target.value)}
            autoFocus
          />
          <input
            placeholder="display name"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
          <div className={styles.creatorRow}>
            <button className={styles.primary} onClick={submit}>Create</button>
            <button
              onClick={() => {
                setCreating(false);
                setNewSlug("");
                setNewName("");
                setError(null);
              }}
            >
              Cancel
            </button>
          </div>
          {error && <div className={styles.error}>{error}</div>}
        </div>
      )}

      {selected && !inSettings && (
        <>
          <div className={styles.sectionHead}>
            <span className={styles.sectionLabel}>Chats</span>
            <button
              className={styles.addBtn}
              title={chatWorkflow ? "New chat" : "Chat workflow not installed"}
              disabled={conn.kind !== "connected" || !chatWorkflow}
              onClick={startChat}
            >
              +
            </button>
          </div>

          <div className={styles.chatTable}>
            {chats.length > 0 && (
              <div className={styles.chatHeader}>
                <span />
                <span>title</span>
                <span>persona</span>
                <span className={styles.colRight}>ctx</span>
                <span />
              </div>
            )}
            {chats.length === 0 && (
              <div className={styles.emptyRow}>No chats yet.</div>
            )}
            {chats.map((s) => {
              const title = s.summary?.title?.trim() || `chat · ${shortId(s.id)}`;
              const persona = s.summary?.persona ?? "—";
              const ctx = s.summary?.context_tokens;
              const isActive = selectedSession === s.id;
              return (
                <div
                  key={s.id}
                  className={`${styles.chatRow} ${isActive ? styles.active : ""}`}
                  onClick={() => selectSession(s.id)}
                  title={tooltipFor(s)}
                >
                  <span
                    className={styles.chatState}
                    data-state={s.state.toLowerCase()}
                    aria-hidden
                  />
                  <span className={styles.chatTitle}>{title}</span>
                  <span className={styles.chatPersona}>{persona}</span>
                  <span className={`${styles.chatCtx} ${styles.colRight}`}>
                    {ctx != null ? formatTokens(ctx) : "—"}
                  </span>
                  <button
                    className={styles.deleteBtn}
                    title="Delete chat"
                    onClick={(e) => {
                      e.stopPropagation();
                      if (confirm(`Delete chat "${title}"? History will be lost.`)) {
                        deleteChat(s.id);
                      }
                    }}
                  >
                    ×
                  </button>
                </div>
              );
            })}
          </div>
        </>
      )}
    </aside>
  );
}

function shortId(id: string): string {
  return id.length > 10 ? id.slice(0, 8) + "…" : id;
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

// Tooltip aggregates the fields we don't render inline so the row
// stays compact. Hovering reveals model, total spend, and message
// count; missing fields are silently skipped.
function tooltipFor(s: SessionInfo): string {
  const lines: string[] = [];
  if (s.summary?.title) lines.push(s.summary.title);
  if (s.summary?.persona) lines.push(`Persona: ${s.summary.persona}`);
  if (s.summary?.model) lines.push(`Model: ${s.summary.model}`);
  if (s.summary?.context_tokens != null) {
    lines.push(`Context: ${formatTokens(s.summary.context_tokens)} tokens`);
  }
  const tp = s.summary?.total_prompt_tokens ?? 0;
  const tc = s.summary?.total_completion_tokens ?? 0;
  if (tp || tc) {
    lines.push(`Total: ${formatTokens(tp)} in / ${formatTokens(tc)} out`);
  }
  if (s.summary?.message_count != null) {
    lines.push(`${s.summary.message_count} messages`);
  }
  if (s.summary?.last_activity) {
    lines.push(`Last activity: ${s.summary.last_activity.slice(0, 16).replace("T", " ")}`);
  }
  return lines.join("\n");
}
