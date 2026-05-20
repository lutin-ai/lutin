// Compact "last active" formatting + date bucketing for the sidebar.
// Kept separate from Sidebar.tsx so the bucket boundaries are easy to
// tweak without scrolling past the layout code.

export type Bucket = "today" | "yesterday" | "thisWeek" | "older";

export const BUCKET_LABEL: Record<Bucket, string> = {
  today: "Today",
  yesterday: "Yesterday",
  thisWeek: "This week",
  older: "Older",
};

// Stable order for rendering — sessions inside each bucket stay sorted
// most-recent-first by the caller, so we only need bucket ordering here.
export const BUCKET_ORDER: Bucket[] = ["today", "yesterday", "thisWeek", "older"];

/** Bucket a wall-clock timestamp relative to `now`. Crossing midnight
 *  matters more than crossing 24h, so "yesterday" is calendar-based,
 *  not 24-to-48-hour. */
export function bucketFor(iso: string, now: Date = new Date()): Bucket {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "older";
  const today = startOfDay(now);
  const that = startOfDay(d);
  const dayDiff = Math.round((today.getTime() - that.getTime()) / 86_400_000);
  if (dayDiff <= 0) return "today";
  if (dayDiff === 1) return "yesterday";
  if (dayDiff < 7) return "thisWeek";
  return "older";
}

/** Compact relative-time chip. Examples:
 *    < 60s  → "now"
 *    < 60m  → "5m"
 *    < 24h  → "3h"
 *    < 7d   → "Mon"     (weekday)
 *    same y → "Mar 14"
 *    older  → "Mar '24" */
export function relativeTime(iso: string, now: Date = new Date()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const diffMs = now.getTime() - d.getTime();
  if (diffMs < 60_000) return "now";
  if (diffMs < 3_600_000) return `${Math.floor(diffMs / 60_000)}m`;
  if (diffMs < 86_400_000) return `${Math.floor(diffMs / 3_600_000)}h`;
  if (diffMs < 7 * 86_400_000) {
    return d.toLocaleDateString(undefined, { weekday: "short" });
  }
  if (d.getFullYear() === now.getFullYear()) {
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
  // Show two-digit year to distinguish older sessions without bloating
  // the chip; `Mar '24` reads cleanly in a narrow sidebar.
  const month = d.toLocaleDateString(undefined, { month: "short" });
  const yy = String(d.getFullYear()).slice(-2);
  return `${month} '${yy}`;
}

function startOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}
