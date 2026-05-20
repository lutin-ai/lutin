// Subsequence match with a small position-aware score. No deps —
// projects/sessions fit in a single picker so a hand-rolled O(n*m)
// scan is fine.
//
// Score components: every matched char is worth 1; a match at the
// start of a word (or the haystack) adds a bonus; consecutive matches
// add a bonus. Higher is better. Returns null when the pattern is
// not a subsequence of the haystack.

export interface FuzzyHit {
  score: number;
  /// Char indices in `haystack` that matched, in order. Useful for
  /// highlighting later — we don't use it yet, but it's cheap to
  /// produce alongside the score.
  indices: number[];
}

export function fuzzyMatch(haystack: string, needle: string): FuzzyHit | null {
  if (!needle) return { score: 0, indices: [] };
  const h = haystack.toLowerCase();
  const n = needle.toLowerCase();
  let hi = 0;
  let ni = 0;
  let score = 0;
  let prevMatch = -2;
  const indices: number[] = [];
  while (hi < h.length && ni < n.length) {
    if (h[hi] === n[ni]) {
      let s = 1;
      if (hi === 0 || h[hi - 1] === " " || h[hi - 1] === "-" || h[hi - 1] === "_" || h[hi - 1] === "/") {
        s += 2;
      }
      if (hi === prevMatch + 1) s += 2;
      score += s;
      indices.push(hi);
      prevMatch = hi;
      ni++;
    }
    hi++;
  }
  if (ni < n.length) return null;
  return { score, indices };
}

export interface RankedItem<T> {
  item: T;
  score: number;
  indices: number[];
}

/// Fuzzy-rank a list against a query, dropping non-matches. When the
/// query is empty, returns items in original order with score 0.
export function fuzzyRank<T>(
  items: T[],
  query: string,
  keyFn: (item: T) => string,
): RankedItem<T>[] {
  const out: RankedItem<T>[] = [];
  for (const item of items) {
    const hit = fuzzyMatch(keyFn(item), query);
    if (hit === null) continue;
    out.push({ item, score: hit.score, indices: hit.indices });
  }
  if (query) out.sort((a, b) => b.score - a.score);
  return out;
}
