import { useMemo, useState } from "react";
import hljs from "highlight.js/lib/common";
import { langFromPath } from "./CodeBlock";

export interface EditToolViewProps {
  path: string;
  oldString: string;
  newString: string;
  /** Tool stdout. We mine the post-edit snippet (lines prefixed with
   *  `   N→` or `   N\t`) for the real file line offset so the gutter
   *  shows actual file positions instead of starting at 1. */
  result?: string;
  /** Unchanged lines kept around each hunk. Default 3 (git default). */
  context?: number;
  /** When true the diff renders collapsed (caller controls header). */
  startCollapsed?: boolean;
}

type DiffOp = { kind: "eq" | "del" | "add"; text: string };

export function EditToolView({
  path,
  oldString,
  newString,
  result,
  context = 3,
  startCollapsed = false,
}: EditToolViewProps) {
  const [collapsed, setCollapsed] = useState(startCollapsed);
  const language = langFromPath(path);
  const startLine = useMemo(
    () => parseEditStartLine(result, newString) ?? 1,
    [result, newString],
  );
  const { hunks } = useMemo(
    () => buildDiff(oldString, newString, context, startLine),
    [oldString, newString, context, startLine],
  );
  return (
    <div className="lutin-diff">
      {!collapsed && (
        <div className="lutin-diff__body">
          {hunks.map((h, hi) => (
            <div key={hi} className="lutin-diff__hunk">
              {hi > 0 && <div className="lutin-diff__sep">…</div>}
              {h.rows.map((row, i) => (
                <DiffRow key={i} row={row} language={language} />
              ))}
            </div>
          ))}
        </div>
      )}
      <button
        type="button"
        className="lutin-write__toggle"
        onClick={(e) => {
          e.stopPropagation();
          setCollapsed((v) => !v);
        }}
      >
        {collapsed ? "Show diff" : "Hide diff"}
      </button>
    </div>
  );
}

/** Cheap count-only diff for header summaries — same line-LCS that
 *  `buildDiff` runs, without materializing hunks. */
export function diffCounts(oldString: string, newString: string): { added: number; removed: number } {
  const ops = lcsDiff(oldString.split("\n"), newString.split("\n"));
  let added = 0;
  let removed = 0;
  for (const op of ops) {
    if (op.kind === "add") added++;
    else if (op.kind === "del") removed++;
  }
  return { added, removed };
}

type HunkRow = {
  kind: "eq" | "del" | "add";
  oldNo: number | null;
  newNo: number | null;
  text: string;
};

function DiffRow({ row, language }: { row: HunkRow; language: string }) {
  const sigil = row.kind === "add" ? "+" : row.kind === "del" ? "−" : " ";
  const html = useMemo(() => {
    const lang = hljs.getLanguage(language) ? language : "plaintext";
    return hljs.highlight(row.text, { language: lang }).value;
  }, [row.text, language]);
  // Single-column gutter: for `eq` both sides matched anyway; for
  // `del`/`add` the present side is the only meaningful number.
  const lineNo = row.newNo ?? row.oldNo ?? "";
  return (
    <div className={`lutin-diff__row lutin-diff__row--${row.kind}`}>
      <span className="lutin-diff__lineno">{lineNo}</span>
      <span className="lutin-diff__sigil">{sigil}</span>
      <code
        className={`hljs language-${language}`}
        dangerouslySetInnerHTML={{ __html: html }}
      />
    </div>
  );
}

interface DiffResult {
  hunks: { rows: HunkRow[] }[];
  added: number;
  removed: number;
}

function buildDiff(
  oldStr: string,
  newStr: string,
  context: number,
  startLine: number,
): DiffResult {
  const a = oldStr.split("\n");
  const b = newStr.split("\n");
  const ops = lcsDiff(a, b);

  // Materialize per-line rows with line numbers from both sides.
  const rows: HunkRow[] = [];
  let oldNo = startLine;
  let newNo = startLine;
  let added = 0;
  let removed = 0;
  for (const op of ops) {
    if (op.kind === "eq") {
      rows.push({ kind: "eq", oldNo, newNo, text: op.text });
      oldNo++;
      newNo++;
    } else if (op.kind === "del") {
      rows.push({ kind: "del", oldNo, newNo: null, text: op.text });
      oldNo++;
      removed++;
    } else {
      rows.push({ kind: "add", oldNo: null, newNo, text: op.text });
      newNo++;
      added++;
    }
  }

  // Group into hunks: a hunk includes every changed row plus `context`
  // unchanged rows on either side. Consecutive hunks whose context
  // windows touch merge into one.
  const changedIdx: number[] = [];
  rows.forEach((r, i) => {
    if (r.kind !== "eq") changedIdx.push(i);
  });
  if (changedIdx.length === 0) return { hunks: [], added, removed };

  const hunks: { rows: HunkRow[] }[] = [];
  let start = Math.max(0, changedIdx[0] - context);
  let end = Math.min(rows.length - 1, changedIdx[0] + context);
  for (let k = 1; k < changedIdx.length; k++) {
    const idx = changedIdx[k];
    const ws = Math.max(0, idx - context);
    if (ws <= end + 1) {
      end = Math.min(rows.length - 1, idx + context);
    } else {
      hunks.push({ rows: rows.slice(start, end + 1) });
      start = ws;
      end = Math.min(rows.length - 1, idx + context);
    }
  }
  hunks.push({ rows: rows.slice(start, end + 1) });
  return { hunks, added, removed };
}

// Recover the file line where the edit landed from the tool's result.
// Two known shapes:
//   * Engine's `file_edit`: `edited <path>:<line>  →  <preview>`
//   * cat -n style snippet: each line prefixed with `   N→` / `   N\t`,
//     in which case the first match of `newString`'s opening line wins,
//     falling back to the snippet's first numbered line.
function parseEditStartLine(
  result: string | undefined,
  newString: string,
): number | null {
  if (!result) return null;
  const edited = /^edited\s+\S+:(\d+)/m.exec(result);
  if (edited) return parseInt(edited[1], 10);

  const re = /^\s*(\d+)[\t→](.*)$/;
  const firstNew = newString.split("\n")[0]?.trim() ?? "";
  let fallback: number | null = null;
  for (const ln of result.split("\n")) {
    const m = re.exec(ln);
    if (!m) continue;
    if (fallback === null) fallback = parseInt(m[1], 10);
    if (firstNew.length > 0 && m[2].trim() === firstNew) {
      return parseInt(m[1], 10);
    }
  }
  return fallback;
}

// Classic LCS table → diff ops. O(n*m) time/space; fine for edit-tool
// payloads which are typically small.
function lcsDiff(a: string[], b: string[]): DiffOp[] {
  const n = a.length;
  const m = b.length;
  const dp: Uint32Array[] = Array.from(
    { length: n + 1 },
    () => new Uint32Array(m + 1),
  );
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      if (a[i] === b[j]) dp[i][j] = dp[i + 1][j + 1] + 1;
      else dp[i][j] = Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out: DiffOp[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ kind: "eq", text: a[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ kind: "del", text: a[i] });
      i++;
    } else {
      out.push({ kind: "add", text: b[j] });
      j++;
    }
  }
  while (i < n) out.push({ kind: "del", text: a[i++] });
  while (j < m) out.push({ kind: "add", text: b[j++] });
  return out;
}
