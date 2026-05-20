import { useState, type ReactNode } from "react";
import type { ToolCallProps } from "../slots";
import { MetricsHeader } from "./MessageBubble";
import { useMessageMenu } from "./MessageActions";
import { WriteToolView } from "./WriteToolView";
import { EditToolView, diffCounts } from "./EditToolView";
import { ShellToolView } from "./ShellToolView";
import { parseReadOutput } from "./CodeBlock";

export function ToolCall({ message, onApprove, onDeny }: ToolCallProps) {
  const [expanded, setExpanded] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const showActions = message.state === "pending" && (onApprove || onDeny);
  const specialView = renderSpecialView(message.name, message.args, message.result);
  const copyText = pickCopyText(message.name, message.args, message.result);
  const menu = useMessageMenu({
    id: message.id,
    text: copyText,
    hasMeta: message.meta != null,
    extraItems: [
      {
        label: collapsed ? "Expand" : "Collapse",
        onSelect: () => setCollapsed((v) => !v),
      },
    ],
  });
  const infoOpen = menu.infoOpen;
  const summary =
    specialView?.headerSummary ??
    (message.args.kind === "parsed"
      ? argPreview(message.args.value)
      : message.args.raw.length > 0
        ? truncate(message.args.raw.replace(/\s+/g, " "), MAX_PREVIEW)
        : null);
  const resultBody =
    message.state === "failed" && message.error
      ? message.error
      : formatBody(message.result);

  // The header is always a button so the whole pill is the click target.
  // Approve/Deny actions live below the body to keep them out of the
  // toggle hit-area; their click handlers stop propagation in case a
  // workflow nests buttons differently.
  const HeadInner = (
    <>
      <span
        className="lutin-chat__tool-dot"
        data-state={message.state}
        aria-hidden="true"
        title={message.state}
      />
      <span className="lutin-chat__tool-name">{message.name}</span>
      {summary && (specialView || !expanded) && (
        <span className="lutin-chat__tool-summary">{summary}</span>
      )}
      {(message.state === "running" || message.state === "pending") && (
        <span className="lutin-chat__tool-state" data-state={message.state}>
          {message.state}
        </span>
      )}
      <MetricsHeader meta={message.meta} />
    </>
  );
  return (
    <div
      className="lutin-chat__msg lutin-chat__msg--tool"
      onContextMenu={menu.onContextMenu}
      {...menu.dataAttrs}
    >
    <div className="lutin-chat__tool" data-state={message.state} data-collapsed={collapsed}>
      {specialView ? (
        <div className="lutin-chat__tool-head lutin-chat__tool-head--static">
          {HeadInner}
        </div>
      ) : (
        <button
          type="button"
          className="lutin-chat__tool-head"
          aria-expanded={expanded}
          onClick={() => setExpanded((v) => !v)}
        >
          {HeadInner}
        </button>
      )}
      {infoOpen && message.meta && !collapsed && <InfoPanel meta={message.meta} />}
      {specialView && !collapsed && (
        <div className="lutin-chat__tool-detail lutin-chat__tool-detail--special">
          {specialView.body}
          {resultBody && message.state === "failed" && (
            <>
              <div className="lutin-chat__tool-label">Error</div>
              <div
                className="lutin-chat__tool-body"
                style={{ color: "var(--chat-err)" }}
              >
                {resultBody}
              </div>
            </>
          )}
        </div>
      )}
      {!specialView && expanded && !collapsed && (
        <div className="lutin-chat__tool-detail">
          {message.args.kind === "parsed" && hasArgs(message.args.value) && (
            <>
              <div className="lutin-chat__tool-label">Input</div>
              <ArgsView value={message.args.value} />
            </>
          )}
          {message.args.kind === "streaming" && message.args.raw.length > 0 && (
            // While the LLM is still streaming the call's input, show
            // the raw partial JSON so users see args fill in live.
            <>
              <div className="lutin-chat__tool-label">Input</div>
              <pre className="lutin-chat__tool-arg-json">{message.args.raw}</pre>
            </>
          )}
          {resultBody && (
            <>
              <div className="lutin-chat__tool-label">
                {message.state === "failed" ? "Error" : "Output"}
              </div>
              <div
                className="lutin-chat__tool-body"
                style={
                  message.state === "failed"
                    ? { color: "var(--chat-err)" }
                    : undefined
                }
              >
                {resultBody}
              </div>
            </>
          )}
        </div>
      )}
      {showActions && !collapsed && (
        <div className="lutin-chat__tool-actions">
          {onApprove && (
            <button
              type="button"
              className="lutin-chat__approve"
              onClick={(e) => {
                e.stopPropagation();
                onApprove(message.id);
              }}
            >
              Approve
            </button>
          )}
          {onDeny && (
            <button
              type="button"
              className="lutin-chat__deny"
              onClick={(e) => {
                e.stopPropagation();
                onDeny(message.id);
              }}
            >
              Deny
            </button>
          )}
        </div>
      )}
    </div>
    {menu.menu}
    </div>
  );
}

// Some tools have bespoke renderings (file Write previews, Edit diffs)
// that replace the generic args/output detail. Returns `null` for
// every other tool; callers fall back to the default ArgsView.
type SpecialView = { body: ReactNode; headerSummary?: string };

function renderSpecialView(
  name: string,
  args: import("../types").ToolCallArgs,
  result: unknown,
): SpecialView | null {
  if (args.kind !== "parsed" || args.value == null || typeof args.value !== "object") {
    return null;
  }
  const obj = args.value as Record<string, unknown>;
  const lower = name.toLowerCase();
  if (lower === "write") {
    const path = readStr(obj, "file_path", "path");
    const content = readStr(obj, "content", "text");
    if (path == null || content == null) return null;
    return {
      headerSummary: `${path} · ${countLines(content)} lines`,
      body: <WriteToolView path={path} content={content} />,
    };
  }
  if (lower === "edit") {
    const path = readStr(obj, "file_path", "path");
    const oldStr = readStr(obj, "old_string", "oldString");
    const newStr = readStr(obj, "new_string", "newString");
    if (path == null || oldStr == null || newStr == null) return null;
    const { added, removed } = diffCounts(oldStr, newStr);
    const resultStr = typeof result === "string" ? result : undefined;
    return {
      headerSummary: `${path}  +${added} −${removed}`,
      body: (
        <EditToolView
          path={path}
          oldString={oldStr}
          newString={newStr}
          result={resultStr}
        />
      ),
    };
  }
  if (lower === "shell" || lower === "bash") {
    const command = readStr(obj, "command", "cmd");
    if (command == null) return null;
    const output = typeof result === "string" ? result : "";
    return {
      headerSummary: command,
      body: <ShellToolView output={output} />,
    };
  }
  if (lower === "read") {
    const path = readStr(obj, "file_path", "path");
    if (path == null) return null;
    const raw = typeof result === "string" ? result : "";
    // Strip `   N→` / `   N\t` line-number prefixes that the Read tool
    // emits, otherwise our own gutter would render the numbers twice.
    const { content, startLine } = parseReadOutput(raw);
    return {
      headerSummary:
        content.length > 0 ? `${path} · ${countLines(content)} lines` : path,
      body: <WriteToolView path={path} content={content} startLine={startLine} />,
    };
  }
  return null;
}

function pickCopyText(
  name: string,
  args: import("../types").ToolCallArgs,
  result: unknown,
): string {
  if (args.kind === "parsed" && args.value != null && typeof args.value === "object") {
    const obj = args.value as Record<string, unknown>;
    const lower = name.toLowerCase();
    if (lower === "write") {
      const c = readStr(obj, "content", "text");
      if (c != null) return c;
    }
    if (lower === "edit") {
      const n = readStr(obj, "new_string", "newString");
      if (n != null) return n;
    }
    if (lower === "read" && typeof result === "string") {
      return result;
    }
    if ((lower === "shell" || lower === "bash") && typeof result === "string") {
      return result;
    }
  }
  // Fallback: the result body, then a stringified args dump.
  if (typeof result === "string") return result;
  if (args.kind === "parsed") {
    try {
      return JSON.stringify(args.value, null, 2);
    } catch {
      return "";
    }
  }
  return args.raw;
}

// Inline metrics readout shown when the user picks "Show info" from
// the context menu. Same data the hover chip would show, just laid out
// vertically so individual rows are scannable.
function InfoPanel({ meta }: { meta: import("../types").MessageMeta }) {
  const rows: Array<[string, string]> = [];
  if (meta.ttftMs != null) rows.push(["TTFT", formatMs(meta.ttftMs)]);
  if (meta.durationMs != null) rows.push(["Duration", formatMs(meta.durationMs)]);
  if (
    meta.completionTokens != null &&
    meta.durationMs != null &&
    meta.durationMs > 0
  ) {
    const tps = (meta.completionTokens * 1000) / meta.durationMs;
    rows.push(["Throughput", `${tps >= 10 ? Math.round(tps) : tps.toFixed(1)} tok/s`]);
  }
  if (meta.promptTokens != null) rows.push(["Input tokens", String(meta.promptTokens)]);
  if (meta.completionTokens != null) rows.push(["Output tokens", String(meta.completionTokens)]);
  if (meta.time) rows.push(["Time", meta.time.toISOString()]);
  if (rows.length === 0) return null;
  return (
    <dl className="lutin-chat__tool-info">
      {rows.map(([k, v]) => (
        <div key={k} className="lutin-chat__tool-info-row">
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

function formatMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(s < 10 ? 2 : 1)} s`;
  const m = Math.floor(s / 60);
  return `${m}m ${Math.round(s % 60)}s`;
}

function readStr(obj: Record<string, unknown>, ...keys: string[]): string | null {
  for (const k of keys) {
    const v = obj[k];
    if (typeof v === "string") return v;
  }
  return null;
}

function countLines(s: string): number {
  if (s.length === 0) return 0;
  return s.split("\n").length;
}

// One-line preview rendered next to the tool name when collapsed.
// Picks the most informative-looking arg field (path/query/url/etc),
// falling back to a short stringified version. Keeps long values from
// blowing up the row by truncating mid-string.
const PREVIEW_KEYS = [
  "file_path",
  "path",
  "filepath",
  "filename",
  "url",
  "query",
  "search",
  "command",
  "cmd",
  "name",
  "id",
  "pattern",
  "prompt",
  "initial_prompt",
];
const MAX_PREVIEW = 80;

function argPreview(value: unknown): string | null {
  if (value == null) return null;
  if (typeof value === "string") return truncate(value, MAX_PREVIEW);
  if (typeof value !== "object") return truncate(String(value), MAX_PREVIEW);
  const obj = value as Record<string, unknown>;
  for (const k of PREVIEW_KEYS) {
    const v = obj[k];
    if (typeof v === "string" && v.length > 0) {
      return truncate(v, MAX_PREVIEW);
    }
  }
  // No known key — show the first scalar value's short form.
  for (const k of Object.keys(obj)) {
    const v = obj[k];
    if (typeof v === "string" || typeof v === "number" || typeof v === "boolean") {
      return truncate(`${k}: ${v}`, MAX_PREVIEW);
    }
  }
  return null;
}

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.slice(0, max - 1) + "…";
}

function formatBody(value: unknown): string | null {
  if (value == null) return null;
  if (typeof value === "string") return value.length === 0 ? null : value;
  // Adapter-projected values are parsed JSON or strings; cycles aren't
  // reachable from the wire, so JSON.stringify can't throw here.
  return JSON.stringify(value, null, 2);
}

function hasArgs(value: unknown): boolean {
  if (value == null) return false;
  if (typeof value === "string") return value.length > 0;
  if (typeof value === "object") {
    if (Array.isArray(value)) return value.length > 0;
    return Object.keys(value as object).length > 0;
  }
  return true;
}

// Render tool args as a labelled table when it's an object — each top-level
// key becomes a row with the field name on the left and the value rendered
// inline (or as a block for multi-line strings / nested structures). For
// non-object inputs (raw string, number, etc.) we fall back to a single
// value block so the renderer is total.
function ArgsView({ value }: { value: unknown }) {
  if (value != null && typeof value === "object" && !Array.isArray(value)) {
    const entries = Object.entries(value as Record<string, unknown>);
    return (
      <dl className="lutin-chat__tool-args">
        {entries.map(([k, v]) => (
          <div key={k} className="lutin-chat__tool-arg">
            <dt className="lutin-chat__tool-arg-key">{k}</dt>
            <dd className="lutin-chat__tool-arg-val">
              <ArgValue value={v} />
            </dd>
          </div>
        ))}
      </dl>
    );
  }
  return (
    <div className="lutin-chat__tool-body">
      <ArgValue value={value} />
    </div>
  );
}

const INLINE_STR_LIMIT = 120;

function ArgValue({ value }: { value: unknown }) {
  if (value === null) {
    return <span className="lutin-chat__tool-arg-null">null</span>;
  }
  if (value === undefined) {
    return <span className="lutin-chat__tool-arg-null">—</span>;
  }
  if (typeof value === "boolean") {
    return <span className="lutin-chat__tool-arg-bool">{String(value)}</span>;
  }
  if (typeof value === "number") {
    return <span className="lutin-chat__tool-arg-num">{value}</span>;
  }
  if (typeof value === "string") {
    if (value.length === 0) {
      return <span className="lutin-chat__tool-arg-null">""</span>;
    }
    const block = value.includes("\n") || value.length > INLINE_STR_LIMIT;
    return (
      <span
        className={
          block
            ? "lutin-chat__tool-arg-str lutin-chat__tool-arg-str--block"
            : "lutin-chat__tool-arg-str"
        }
      >
        {value}
      </span>
    );
  }
  if (Array.isArray(value)) {
    if (value.length === 0) {
      return <span className="lutin-chat__tool-arg-null">[]</span>;
    }
    const allScalar = value.every(
      (v) =>
        v === null ||
        typeof v === "string" ||
        typeof v === "number" ||
        typeof v === "boolean",
    );
    if (allScalar) {
      return (
        <span className="lutin-chat__tool-arg-list">
          {value.map((v, i) => (
            <span key={i} className="lutin-chat__tool-arg-chip">
              <ArgValue value={v} />
            </span>
          ))}
        </span>
      );
    }
    return (
      <pre className="lutin-chat__tool-arg-json">
        {JSON.stringify(value, null, 2)}
      </pre>
    );
  }
  if (typeof value === "object") {
    return (
      <pre className="lutin-chat__tool-arg-json">
        {JSON.stringify(value, null, 2)}
      </pre>
    );
  }
  return <span>{String(value)}</span>;
}
