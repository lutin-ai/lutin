// Sidecar audit view for the principled workflow. Renders every
// reviewer verdict in newest-first order, filterable by verdict kind.
// Source of truth is `snap.reviewLog` — replaced wholesale on the
// `reviews` ChatOk after Subscribe, then appended live as each
// `reviewerCompleted` event lands. Frame resolution events are
// inferred client-side by walking the log; the engine doesn't write
// "step accepted" rows because acceptance is the absence of a fail.

import { memo, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { ReviewLogEntry, ReviewVerdict } from "@lutin/principled-protocol";
import styles from "./ReviewerSidebar.module.css";

interface Props {
  log: ReviewLogEntry[];
}

const WIDTH_KEY = "lutin.principled.reviewerSidebar.width";
const MIN_W = 220;
const MAX_W = 720;
const DEFAULT_W = 320;
const EST_ROW_PX = 64;

function loadInitialWidth(): number {
  const raw = localStorage.getItem(WIDTH_KEY);
  const n = raw ? Number(raw) : NaN;
  if (!Number.isFinite(n)) return DEFAULT_W;
  return Math.min(MAX_W, Math.max(MIN_W, n));
}

/// Verdict bucket the row falls into. The filter chip uses the
/// `"all"` superset; helpers that classify a single verdict return
/// `RowBucket` so the exhaustiveness check in `verdictClass` stays
/// honest.
type RowBucket = "pass" | "nit" | "fix" | "rethink";
type VerdictBucket = "all" | RowBucket;

const BUCKETS: { id: VerdictBucket; label: string }[] = [
  { id: "all", label: "all" },
  { id: "pass", label: "pass" },
  { id: "nit", label: "nit" },
  { id: "fix", label: "fix" },
  { id: "rethink", label: "rethink" },
];

export function ReviewerSidebar({ log }: Props) {
  const [bucket, setBucket] = useState<VerdictBucket>("all");
  const filtered = useMemo(() => filterLog(log, bucket), [log, bucket]);

  const [width, setWidth] = useState<number>(() => loadInitialWidth());
  const [dragging, setDragging] = useState(false);
  const dragStartRef = useRef<{ x: number; w: number } | null>(null);

  const listRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => EST_ROW_PX,
    overscan: 8,
    getItemKey: (i) => `${filtered[i].stepId}-${filtered[i].reviewerCallId}`,
  });
  const totalSize = virtualizer.getTotalSize();

  useEffect(() => {
    if (!dragging) return;
    const onMove = (e: MouseEvent) => {
      const start = dragStartRef.current;
      if (!start) return;
      // Sidebar is on the right edge — dragging left grows it. Subtract
      // the delta so the visual edge tracks the cursor.
      const next = Math.min(
        MAX_W,
        Math.max(MIN_W, start.w - (e.clientX - start.x)),
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
    localStorage.setItem(WIDTH_KEY, String(width));
  }, [width]);

  const startResize = (e: React.MouseEvent) => {
    e.preventDefault();
    dragStartRef.current = { x: e.clientX, w: width };
    setDragging(true);
  };

  const items = virtualizer.getVirtualItems();

  return (
    <div className={styles.root} style={{ width }}>
      <div
        className={`${styles.resizer} ${dragging ? styles.dragging : ""}`}
        onMouseDown={startResize}
        role="separator"
        aria-orientation="vertical"
      />
      <div className={styles.header}>
        <span className={styles.title}>Reviewers</span>
        <span className={styles.count}>{log.length}</span>
      </div>
      <div className={styles.filters}>
        {BUCKETS.map((b) => (
          <button
            key={b.id}
            type="button"
            className={styles.chip}
            data-active={bucket === b.id}
            onClick={() => setBucket(b.id)}
          >
            {b.label}
          </button>
        ))}
      </div>
      <div ref={listRef} className={styles.list}>
        {filtered.length === 0 ? (
          <div className={styles.empty}>
            {log.length === 0
              ? "No reviewer activity yet."
              : "No rows match this filter."}
          </div>
        ) : (
          <div className={styles.virt} style={{ height: totalSize }}>
            {items.map((vi) => {
              const row = filtered[vi.index];
              return (
                <div
                  key={vi.key}
                  data-index={vi.index}
                  ref={virtualizer.measureElement}
                  className={styles.vrow}
                  style={{ top: vi.start }}
                >
                  <Row row={row} />
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

const Row = memo(function Row({ row }: { row: ReviewLogEntry }) {
  return (
    <div className={styles.row}>
      <div className={styles.rowHead}>
        <span className={styles.principle}>{row.principle}</span>
        <span className={styles.ts}>{shortTs(row.ts)}</span>
      </div>
      <div className={styles.tool}>
        {row.toolName}
        {row.argsSummary ? `(${row.argsSummary})` : ""}
      </div>
      <span className={`${styles.verdict} ${verdictClass(row.verdict)}`}>
        {verdictLabel(row.verdict)}
      </span>
      {verdictReasoning(row.verdict) && (
        <div className={styles.reasoning}>{verdictReasoning(row.verdict)}</div>
      )}
    </div>
  );
});

function filterLog(log: ReviewLogEntry[], bucket: VerdictBucket): ReviewLogEntry[] {
  // Newest-first display in a single pass: walk the engine-ordered log
  // back-to-front and push matches directly.
  const out: ReviewLogEntry[] = [];
  const all = bucket === "all";
  for (let i = log.length - 1; i >= 0; i--) {
    const row = log[i];
    if (all || verdictBucket(row.verdict) === bucket) out.push(row);
  }
  return out;
}

function verdictBucket(v: ReviewVerdict): RowBucket {
  if (v.kind === "pass") return "pass";
  if (v.kind === "passWithNit") return "nit";
  return v.severity.kind;
}

function verdictLabel(v: ReviewVerdict): string {
  if (v.kind === "pass") return "pass";
  if (v.kind === "passWithNit") return "nit";
  return v.severity.kind;
}

/// Map of bucket → CSS module class. Indexing instead of switching
/// keeps the exhaustiveness guarantee on the type system: a future
/// fifth `RowBucket` variant is a missing record key, which TS flags
/// at compile time. (A `default:` arm would have hidden the gap.)
const VERDICT_CLASS: Record<RowBucket, string> = {
  pass: styles.verdictPass,
  nit: styles.verdictNit,
  fix: styles.verdictFix,
  rethink: styles.verdictRethink,
};

function verdictClass(v: ReviewVerdict): string {
  return VERDICT_CLASS[verdictBucket(v)];
}

function verdictReasoning(v: ReviewVerdict): string | null {
  if (v.kind === "pass") return null;
  if (v.kind === "passWithNit") return v.reasoning;
  return v.reasoning;
}

function shortTs(ts: string): string {
  // Engine writes RFC3339; we just want HH:MM:SS for the row chip. If
  // the parser fails (legacy/garbled row) fall back to the raw string
  // truncated to 8 chars so the row still renders something usable.
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts.slice(0, 8);
  return d.toLocaleTimeString(undefined, { hour12: false });
}
