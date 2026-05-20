import { useEffect, useRef, useState } from "react";
import { cpSendOk } from "../api";
import { useApp } from "../store";
import type {
  SessionId,
  SessionInfo,
  SubAgentRow,
  WorkflowId,
  WorkflowInfo,
} from "../types";
import styles from "./Sidebar.module.css";
import { WorkflowIcon } from "./WorkflowIcon";
import { RowMenu, type RowMenuItem } from "./RowMenu";
import { BUCKET_LABEL, BUCKET_ORDER, bucketFor, relativeTime, type Bucket } from "./sessionTime";

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
  const selected = useApp((s) => s.selectedProject);
  const view = useApp((s) => s.view);
  const conn = useApp((s) => s.conn);
  const sessionsBySlug = useApp((s) => s.sessionsBySlug);
  const setSessions = useApp((s) => s.setSessions);
  const selectedSession = useApp((s) => s.selectedSession);
  const selectSession = useApp((s) => s.selectSession);
  const applyEvent = useApp((s) => s.applyEvent);
  const removeSession = useApp((s) => s.removeSession);
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

  const [error, setError] = useState<string | null>(null);
  const [workflows, setWorkflows] = useState<Record<string, WorkflowInfo>>({});
  // Per-row right-click menu. One open at a time; row id is stashed
  // alongside viewport coords. Click-outside / Escape close via the
  // <RowMenu> component itself.
  const [rowMenu, setRowMenu] = useState<
    | { id: SessionId; title: string; x: number; y: number }
    | null
  >(null);
  // Sub-agent trees collapse by default; only opted-in rows render
  // children. The chip on each row toggles membership in this set.
  const [expandedAgents, setExpandedAgents] = useState<Set<SessionId>>(new Set());
  // Cheap clock-tick so relative timestamps re-render without
  // requiring a session update. One minute is enough — sub-minute
  // values say "now" anyway.
  const [, setClockTick] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setClockTick((n) => n + 1), 60_000);
    return () => window.clearInterval(id);
  }, []);

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
      // SessionEnded only marks Dormant now (so reap doesn't hide rows);
      // a real delete needs to drop the row explicitly.
      removeSession(selected, id);
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
      {error && <div className={styles.error}>{error}</div>}

      {selected && !inSettings && buildSidebarSections(workflows, sessionsByWorkflow).map(({ id: wfId, label, icon, installed }) => {
        const sessions = sessionsByWorkflow[wfId] ?? [];
        return (
          <div key={wfId}>
          <div className={styles.sectionHead}>
            <span className={styles.sectionLabel}>
              <WorkflowIcon id={wfId} fallback={icon} />
              {label}
            </span>
            <button
              className={styles.addBtn}
              title={installed ? `New ${label}` : `${label} workflow not installed`}
              disabled={conn.kind !== "connected" || !installed}
              onClick={() => startSession(wfId)}
            >
              +
            </button>
          </div>

          <div className={styles.chatTable}>
            {sessions.length === 0 && (
              <div className={styles.emptyRow}>No sessions yet.</div>
            )}
            {bucketSessions(sessions).map(({ bucket, rows }) => (
              <div key={bucket} className={styles.bucket}>
                <div className={styles.bucketLabel}>{BUCKET_LABEL[bucket]}</div>
                {rows.map((s) => {
                  const title = s.summary?.title?.trim() || `${wfId} · ${shortId(s.id)}`;
                  const isActive = selectedSession === s.id;
                  const chatState = chatStateBySession[s.id];
                  const subAgents = chatState?.agents ?? [];
                  const subSelected = chatState?.selected ?? null;
                  const parentSelected = isActive && subSelected === null;
                  const lastIso = s.summary?.last_activity ?? s.created_at;
                  const subCount = subAgents.length;
                  const expanded = expandedAgents.has(s.id);
                  return (
                    <div key={s.id}>
                      <div
                        className={`${styles.chatRow} ${parentSelected ? styles.active : ""}`}
                        onClick={() => {
                          selectSession(s.id);
                          if (subSelected !== null) selectSubAgent(s.id, null);
                        }}
                        onContextMenu={(e) => {
                          e.preventDefault();
                          setRowMenu({ id: s.id, title, x: e.clientX, y: e.clientY });
                        }}
                        title={tooltipFor(s)}
                      >
                        <span
                          className={styles.chatState}
                          data-state={s.state.toLowerCase()}
                          aria-hidden
                        />
                        <span className={styles.chatTitle}>{title}</span>
                        {subCount > 0 && (
                          <button
                            type="button"
                            className={styles.subChip}
                            data-expanded={expanded || undefined}
                            title={expanded ? "Hide sub-agents" : `Show ${subCount} sub-agent${subCount === 1 ? "" : "s"}`}
                            aria-expanded={expanded}
                            onClick={(e) => {
                              e.stopPropagation();
                              setExpandedAgents((prev) => {
                                const next = new Set(prev);
                                if (next.has(s.id)) next.delete(s.id);
                                else next.add(s.id);
                                return next;
                              });
                            }}
                          >
                            <CogIcon />
                            {subCount}
                          </button>
                        )}
                        <span className={styles.chatTime}>{relativeTime(lastIso)}</span>
                      </div>
                      {expanded && subAgents.length > 0 && (
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
            ))}
          </div>
          </div>
        );
      })}
      {rowMenu && (
        <RowMenu
          pos={{ x: rowMenu.x, y: rowMenu.y }}
          items={buildSessionMenu(rowMenu, deleteSession)}
          onClose={() => setRowMenu(null)}
        />
      )}
    </aside>
  );
}

function buildSessionMenu(
  menu: { id: SessionId; title: string },
  deleteSession: (id: SessionId) => void,
): RowMenuItem[] {
  return [
    {
      label: "Delete",
      danger: true,
      onSelect: () => {
        if (confirm(`Delete "${menu.title}"? History will be lost.`)) {
          deleteSession(menu.id);
        }
      },
    },
  ];
}

// Group already-time-sorted sessions into the four buckets the sidebar
// renders. Empty buckets are dropped so the rendered list has no gaps.
function bucketSessions(sessions: SessionInfo[]): Array<{ bucket: Bucket; rows: SessionInfo[] }> {
  const now = new Date();
  const groups: Record<Bucket, SessionInfo[]> = {
    today: [],
    yesterday: [],
    thisWeek: [],
    older: [],
  };
  for (const s of sessions) {
    const iso = s.summary?.last_activity ?? s.created_at;
    groups[bucketFor(iso, now)].push(s);
  }
  return BUCKET_ORDER.filter((b) => groups[b].length > 0).map((b) => ({
    bucket: b,
    rows: groups[b],
  }));
}

function CogIcon() {
  return (
    <svg width="11" height="11" viewBox="0 0 14 14" fill="none" aria-hidden>
      <circle cx="7" cy="7" r="2" stroke="currentColor" strokeWidth="1.2" />
      <path
        d="M7 1.5v1.6M7 10.9v1.6M1.5 7h1.6M10.9 7h1.6M2.9 2.9l1.1 1.1M10 10l1.1 1.1M2.9 11.1L4 10M10 4l1.1-1.1"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinecap="round"
      />
    </svg>
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

interface SidebarSection {
  id: WorkflowId;
  label: string;
  icon: string | null;
  installed: boolean;
}

// One sidebar section per installed workflow, plus a fallback section
// for any workflow id that owns sessions but is no longer installed —
// so the user can still see and delete dormant sessions when an image
// is removed. Installed sections sort alphabetically by display name;
// orphan sections trail, sorted by id.
function buildSidebarSections(
  workflows: Record<string, WorkflowInfo>,
  sessionsByWorkflow: Record<string, SessionInfo[]>,
): SidebarSection[] {
  const installed: SidebarSection[] = Object.values(workflows).map((w) => ({
    id: w.id,
    label: w.display_name,
    icon: w.icon || null,
    installed: true,
  }));
  installed.sort((a, b) => a.label.localeCompare(b.label));

  const orphans: SidebarSection[] = Object.keys(sessionsByWorkflow)
    .filter((id) => !(id in workflows))
    .sort()
    .map((id) => ({ id, label: id, icon: null, installed: false }));

  return [...installed, ...orphans];
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
