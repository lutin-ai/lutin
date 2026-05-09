import { useState } from "react";
import type { ToolCallProps } from "../slots";
import { MetricsHeader } from "./MessageBubble";

export function ToolCall({ message, onApprove, onDeny }: ToolCallProps) {
  const [expanded, setExpanded] = useState(false);
  const showActions = message.state === "pending" && (onApprove || onDeny);
  const summary =
    message.args.kind === "parsed"
      ? argPreview(message.args.value)
      : message.args.raw.length > 0
        ? truncate(message.args.raw.replace(/\s+/g, " "), MAX_PREVIEW)
        : null;
  const resultBody =
    message.state === "failed" && message.error
      ? message.error
      : formatBody(message.result);

  // The header is always a button so the whole pill is the click target.
  // Approve/Deny actions live below the body to keep them out of the
  // toggle hit-area; their click handlers stop propagation in case a
  // workflow nests buttons differently.
  return (
    <div className="lutin-chat__msg lutin-chat__msg--tool">
    <div className="lutin-chat__tool" data-state={message.state}>
      <button
        type="button"
        className="lutin-chat__tool-head"
        aria-expanded={expanded}
        onClick={() => setExpanded((v) => !v)}
      >
        <span
          className="lutin-chat__tool-dot"
          data-state={message.state}
          aria-hidden="true"
          title={message.state}
        />
        <span className="lutin-chat__tool-name">{message.name}</span>
        {summary && !expanded && (
          <span className="lutin-chat__tool-summary">{summary}</span>
        )}
        {(message.state === "running" || message.state === "pending") && (
          <span className="lutin-chat__tool-state" data-state={message.state}>
            {message.state}
          </span>
        )}
        <MetricsHeader meta={message.meta} />
      </button>
      {expanded && (
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
      {showActions && (
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
    </div>
  );
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
