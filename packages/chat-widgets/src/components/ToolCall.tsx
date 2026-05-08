import { useState } from "react";
import type { ToolCallProps } from "../slots";
import { MetricsHeader } from "./MessageBubble";

export function ToolCall({ message, onApprove, onDeny }: ToolCallProps) {
  const [expanded, setExpanded] = useState(false);
  const showActions = message.state === "pending" && (onApprove || onDeny);
  const summary = argPreview(message.args);
  const argBody = formatBody(message.args);
  const resultBody =
    message.state === "failed" && message.error
      ? message.error
      : formatBody(message.result);

  // The header is always a button so the whole pill is the click target.
  // Approve/Deny actions live below the body to keep them out of the
  // toggle hit-area; their click handlers stop propagation in case a
  // workflow nests buttons differently.
  return (
    <div className="lutin-chat__tool" data-state={message.state}>
      <button
        type="button"
        className="lutin-chat__tool-head"
        aria-expanded={expanded}
        onClick={() => setExpanded((v) => !v)}
      >
        <span className="lutin-chat__tool-caret" aria-hidden="true">
          {expanded ? "▾" : "▸"}
        </span>
        <span className="lutin-chat__tool-name">{message.name}</span>
        {summary && !expanded && (
          <span className="lutin-chat__tool-summary">{summary}</span>
        )}
        <span className="lutin-chat__tool-state" data-state={message.state}>
          {message.state}
        </span>
        <MetricsHeader meta={message.meta} />
      </button>
      {expanded && (
        <div className="lutin-chat__tool-detail">
          {argBody && (
            <>
              <div className="lutin-chat__tool-label">Input</div>
              <div className="lutin-chat__tool-body">{argBody}</div>
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
