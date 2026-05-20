// Compact relative-time formatter: 2m, 5h, 3d, then absolute date.
// Input is an RFC3339 string from the engine (`created_at` /
// `summary.last_activity`); invalid or missing returns the empty
// string so callers can render an em-dash placeholder themselves.

export function relativeTime(iso: string | null | undefined, now: number = Date.now()): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "";
  const diff = Math.max(0, now - t) / 1000;
  if (diff < 60) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  if (diff < 86400 * 7) return `${Math.floor(diff / 86400)}d`;
  const d = new Date(t);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return d.toLocaleDateString(undefined, sameYear
    ? { month: "short", day: "numeric" }
    : { year: "numeric", month: "short", day: "numeric" });
}

/// Most-recent activity across a session list, as ISO. `null` when
/// none — useful for projects with no sessions yet.
export function latestActivity(sessions: { created_at: string; summary?: { last_activity?: string | null } | null }[]): string | null {
  let best: string | null = null;
  for (const s of sessions) {
    const t = s.summary?.last_activity ?? s.created_at;
    if (!t) continue;
    if (best === null || t > best) best = t;
  }
  return best;
}
