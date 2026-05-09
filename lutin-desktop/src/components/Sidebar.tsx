import { useEffect, useRef, useState } from "react";
import { cpSendOk } from "../api";
import { useApp } from "../store";
import type {
  SessionId,
  SessionInfo,
  SubAgentRow,
  WorkflowInfo,
} from "../types";
import styles from "./Sidebar.module.css";

// Sidebar surfaces one section per known workflow so the user can
// start sessions without a picker. Each entry maps a workflow id to
// the section label shown above its session list. Order here drives
// section order in the sidebar.
const WORKFLOW_SECTIONS: Array<{ id: string; label: string }> = [
  { id: "chat", label: "Chats" },
  { id: "principled", label: "Principled" },
  { id: "image", label: "Images" },
];
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
  const chatStateBySession = useApp((s) => s.chatStateBySession);
  const selectSubAgent = useApp((s) => s.selectSubAgent);

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
  const [workflows, setWorkflows] = useState<Record<string, WorkflowInfo>>({});

  // Resolve installed workflows once on connect so each section's
  // "+" button can start a session without a picker. Missing images
  // hide their "+" rather than show an error.
  useEffect(() => {
    if (conn.kind !== "connected") return;
    let cancelled = false;
    cpSendOk("ListWorkflows")
      .then((r) => {
        if (cancelled) return;
        if (typeof r === "object" && "Workflows" in r) {
          const map: Record<string, WorkflowInfo> = {};
          for (const w of r.Workflows) map[w.id] = w;
          setWorkflows(map);
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

  // Sessions for the active project, grouped by workflow id and
  // sorted most-recently-active first within each group. Computed
  // once so each section reads its slice without re-filtering.
  const sessionsByWorkflow: Record<string, SessionInfo[]> = (() => {
    if (!selected || inSettings) return {};
    const all = sessionsBySlug[selected] ?? [];
    const groups: Record<string, SessionInfo[]> = {};
    for (const s of all) {
      (groups[s.workflow] ??= []).push(s);
    }
    for (const list of Object.values(groups)) {
      list.sort((a, b) => {
        const ka = a.summary?.last_activity ?? a.created_at;
        const kb = b.summary?.last_activity ?? b.created_at;
        return kb.localeCompare(ka);
      });
    }
    return groups;
  })();

  const startSession = async (workflowId: string) => {
    if (!selected) return;
    setError(null);
    try {
      const r = await cpSendOk({
        StartSession: { slug: selected, workflow: workflowId },
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

  const deleteSession = async (id: string) => {
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

      {selected && !inSettings && WORKFLOW_SECTIONS.map(({ id: wfId, label }) => {
        const wf = workflows[wfId];
        const sessions = sessionsByWorkflow[wfId] ?? [];
        return (
          <div key={wfId}>
          <div className={styles.sectionHead}>
            <span className={styles.sectionLabel}>{label}</span>
            <button
              className={styles.addBtn}
              title={wf ? `New ${label.toLowerCase()}` : `${label} workflow not installed`}
              disabled={conn.kind !== "connected" || !wf}
              onClick={() => startSession(wfId)}
            >
              +
            </button>
          </div>

          <div className={styles.chatTable}>
            {sessions.length > 0 && (
              <div className={styles.chatHeader}>
                <span />
                <span>title</span>
                <span>persona</span>
                <span className={styles.colRight}>ctx</span>
                <span />
              </div>
            )}
            {sessions.length === 0 && (
              <div className={styles.emptyRow}>No sessions yet.</div>
            )}
            {sessions.map((s) => {
              const title = s.summary?.title?.trim() || `${wfId} · ${shortId(s.id)}`;
              const persona = s.summary?.persona ?? "—";
              const ctx = s.summary?.context_tokens;
              const isActive = selectedSession === s.id;
              // Sub-agents only flow in for the session whose iframe
              // is mounted (today: the selected chat). Other chats
              // render as plain rows. The selected sub-agent (if any)
              // visually de-highlights the parent chat row so the
              // selection stays unambiguous.
              const chatState = chatStateBySession[s.id];
              const subAgents = chatState?.agents ?? [];
              const subSelected = chatState?.selected ?? null;
              const parentSelected = isActive && subSelected === null;
              return (
                <div key={s.id}>
                  <div
                    className={`${styles.chatRow} ${parentSelected ? styles.active : ""}`}
                    onClick={() => {
                      selectSession(s.id);
                      // Clicking the parent chat row deselects any
                      // child agent — there's no separate "back"
                      // affordance in the iframe anymore.
                      if (subSelected !== null) selectSubAgent(s.id, null);
                    }}
                    title={tooltipFor(s)}
                  >
                    <span
                      className={styles.chatState}
                      data-state={s.state.toLowerCase()}
                      aria-hidden
                    />
                    <span className={styles.chatTitle}>{title}</span>
                    <span className={styles.chatPersona}>{persona}</span>
                    <span
                      className={`${styles.chatCtx} ${styles.colRight}`}
                      data-band={ctx != null ? ctxBand(ctx) : undefined}
                    >
                      {ctx != null ? formatTokens(ctx) : "—"}
                    </span>
                    <button
                      className={styles.deleteBtn}
                      title="Delete session"
                      onClick={(e) => {
                        e.stopPropagation();
                        if (confirm(`Delete "${title}"? History will be lost.`)) {
                          deleteSession(s.id);
                        }
                      }}
                    >
                      ×
                    </button>
                  </div>
                  {subAgents.length > 0 && (
                    <SubAgentRows
                      session={s.id}
                      agents={subAgents}
                      selected={subSelected}
                      onSelect={(id) => {
                        selectSession(s.id);
                        selectSubAgent(s.id, id);
                      }}
                    />
                  )}
                </div>
              );
            })}
          </div>
          </div>
        );
      })}
    </aside>
  );
}

// Maximum visual indent depth for sub-agent rows. Direct children of
// the parent chat sit at depth 1, grandchildren at 2; anything
// deeper is clamped to depth 3 so the tree stays scannable in the
// narrow sidebar. Rows past this depth still render — they just
// stop accumulating indent and tree connectors lose precision.
const MAX_SUBAGENT_DEPTH = 3;

interface SubAgentRowsProps {
  session: SessionId;
  agents: SubAgentRow[];
  selected: string | null;
  onSelect: (id: string) => void;
}

function SubAgentRows({ agents, selected, onSelect }: SubAgentRowsProps) {
  const rows = flattenSubAgents(agents);
  return (
    <div className={styles.subAgentList}>
      {rows.map((row) => {
        const depth = Math.min(row.depth, MAX_SUBAGENT_DEPTH);
        const indent = (depth - 1) * 12;
        const connector = row.isLast ? "└─" : "├─";
        const isSelected = selected === row.agent.id;
        const title =
          row.agent.status.kind === "failed"
            ? row.agent.status.reason
            : (row.agent.lastProgress ?? row.agent.persona);
        return (
          <div
            key={row.agent.id}
            className={`${styles.subAgentRow} ${isSelected ? styles.active : ""}`}
            onClick={() => onSelect(row.agent.id)}
            title={title}
            style={{ paddingLeft: 8 + indent }}
          >
            <span className={styles.subAgentConnector}>{connector}</span>
            <span
              className={styles.subAgentDot}
              data-status={row.agent.status.kind}
              aria-hidden
            />
            <span className={styles.subAgentId}>{row.agent.id}</span>
            <span className={styles.subAgentPersona}>{row.agent.persona}</span>
          </div>
        );
      })}
    </div>
  );
}

interface FlatSubAgent {
  agent: SubAgentRow;
  /// 1-based depth from the parent chat (direct children = 1).
  depth: number;
  /// True when the row is the last sibling at its level. Drives the
  /// `└─` vs `├─` connector glyph.
  isLast: boolean;
}

function flattenSubAgents(agents: SubAgentRow[]): FlatSubAgent[] {
  // Group agents by parentId so the recursion has O(1) child lookup.
  // Orphans (parentId references an agent we don't have a row for)
  // get hoisted to the top level — better to render them than to
  // silently drop a live agent because of a transient ordering race.
  const ids = new Set(agents.map((a) => a.id));
  const childrenOf = new Map<string | null, SubAgentRow[]>();
  for (const a of agents) {
    const parent = a.parentId !== null && ids.has(a.parentId) ? a.parentId : null;
    const list = childrenOf.get(parent);
    if (list) list.push(a);
    else childrenOf.set(parent, [a]);
  }
  const out: FlatSubAgent[] = [];
  const walk = (parent: string | null, depth: number) => {
    const kids = childrenOf.get(parent) ?? [];
    kids.forEach((kid, i) => {
      out.push({ agent: kid, depth, isLast: i === kids.length - 1 });
      walk(kid.id, depth + 1);
    });
  };
  walk(null, 1);
  return out;
}

function shortId(id: string): string {
  return id.length > 10 ? id.slice(0, 8) + "…" : id;
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

/** Color band for the ctx column. Kept in lockstep with the chat
 *  composer's `SummaryFooter::bandFor` so both surfaces agree on
 *  what counts as "comfortable / busy / hot". */
function ctxBand(tokens: number): "low" | "mid" | "high" {
  if (tokens <= 50_000) return "low";
  if (tokens <= 100_000) return "mid";
  return "high";
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
