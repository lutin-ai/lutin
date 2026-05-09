// One-line dim footer rendered below a chat composer shell.
// Surfaces the live ctx fill + cumulative provider tokens so the
// user can watch token spend without leaving the chat surface.
// Workflow-agnostic: each engine pushes its own `summaryUpdated`
// stream and feeds the resulting numbers in here.

import styles from "./SummaryFooter.module.css";

export interface SummaryFooterProps {
  /** Live counters projected from the workflow's most recent
   *  `summaryUpdated` broadcast. `null` until the first usage report
   *  arrives — em-dash placeholders keep the layout stable. */
  summary: {
    contextTokens: number | null;
    totalPromptTokens: number;
    totalCompletionTokens: number;
  } | null;
}

export function SummaryFooter({ summary }: SummaryFooterProps) {
  const ctxValue = summary?.contextTokens ?? null;
  const ctx = ctxValue != null ? formatTokens(ctxValue) : "—";
  const totalIn = summary ? formatTokens(summary.totalPromptTokens) : "—";
  const totalOut = summary ? formatTokens(summary.totalCompletionTokens) : "—";
  const ctxBand = ctxValue != null ? bandFor(ctxValue) : "idle";
  return (
    <div
      className={styles.summary}
      title="Context fill (green ≤50k · yellow ≤100k · red >100k) · cumulative input · cumulative output"
    >
      <span className={styles.ctx} data-band={ctxBand}>
        <span className={styles.ctxDot} aria-hidden />
        <span className={styles.ctxLabel}>ctx</span>
        <span className={styles.ctxValue}>{ctx}</span>
      </span>
      <span className={styles.summarySep} aria-hidden />
      <span className={styles.metric} title="Cumulative input tokens billed this session">
        <ArrowUp />
        <span className={styles.metricLabel}>in</span>
        <span className={styles.metricValue}>{totalIn}</span>
      </span>
      <span className={styles.summarySep} aria-hidden />
      <span className={styles.metric} title="Cumulative output tokens billed this session">
        <ArrowDown />
        <span className={styles.metricLabel}>out</span>
        <span className={styles.metricValue}>{totalOut}</span>
      </span>
    </div>
  );
}

/** k/M-suffixed compact token formatter. The desktop sidebar's
 *  `formatTokens` lives in `lutin-desktop/src/components/Sidebar.tsx`
 *  and uses the same rounding rules — keep the two in sync. */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

/** Color band thresholds for the live ctx readout. Matches the
 *  desktop sidebar's `ctxBand` so the two surfaces agree on what
 *  counts as "comfortable / busy / hot". Kept in lockstep with
 *  `Sidebar.tsx::ctxBand`. */
export function bandFor(tokens: number): "low" | "mid" | "high" {
  if (tokens <= 50_000) return "low";
  if (tokens <= 100_000) return "mid";
  return "high";
}

function ArrowUp() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path
        d="M5 8.5V1.5M2 4l3-3 3 3"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function ArrowDown() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path
        d="M5 1.5v7M2 6l3 3 3-3"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
