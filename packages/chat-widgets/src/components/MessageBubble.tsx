import { useEffect, useRef, useState } from "react";
import { Markdown } from "../markdown";
import type {
  AgentMessageProps,
  AssistantMessageProps,
  SystemMessageProps,
  UserMessageProps,
} from "../slots";
import type { MessageMeta } from "../types";
import { useMessageMenu } from "./MessageActions";

/// Right-aligned metrics chip rendered inside a `.lutin-chat__msg-meta`
/// row. Renders nothing when `meta` is undefined, so callers can drop
/// it in unconditionally without guarding. Order: TTFT → duration →
/// tok/s → in/out tokens → clock time. Each piece is suppressed
/// individually so a partial metric set still produces a tidy line.
export function MetricsHeader({ meta }: { meta?: MessageMeta }) {
  if (!meta) return null;
  const parts: string[] = [];
  if (meta.ttftMs !== null && meta.ttftMs !== undefined) {
    parts.push(`TTFT ${formatDuration(meta.ttftMs)}`);
  }
  if (meta.durationMs !== null && meta.durationMs !== undefined) {
    parts.push(formatDuration(meta.durationMs));
  }
  if (
    meta.completionTokens !== null &&
    meta.completionTokens !== undefined &&
    meta.durationMs !== null &&
    meta.durationMs !== undefined &&
    meta.durationMs > 0
  ) {
    const tps = (meta.completionTokens * 1000) / meta.durationMs;
    parts.push(`${tps >= 10 ? Math.round(tps) : tps.toFixed(1)} tok/s`);
  }
  if (meta.promptTokens !== null && meta.promptTokens !== undefined) {
    parts.push(`${formatTokens(meta.promptTokens)} in`);
  }
  if (meta.completionTokens !== null && meta.completionTokens !== undefined) {
    parts.push(`${formatTokens(meta.completionTokens)} out`);
  }
  const ts = formatTime(meta.time);
  if (ts) parts.push(ts);
  if (parts.length === 0) return null;
  return (
    <span
      className="lutin-chat__msg-metrics"
      title={meta.time?.toISOString()}
    >
      {parts.join(" · ")}
    </span>
  );
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(s < 10 ? 2 : 1)}s`;
  const m = Math.floor(s / 60);
  const rem = Math.round(s % 60);
  return `${m}m ${rem}s`;
}

function formatTokens(n: number): string {
  if (n < 1000) return `${n}`;
  return `${(n / 1000).toFixed(n < 10_000 ? 2 : 1)}k`;
}

/// Render a wall-clock chip. Same-day messages get HH:MM; older
/// messages get YYYY-MM-DD HH:MM. The "is today?" check reads the
/// system clock, which is the only impure bit of MetricsHeader —
/// localized here so a future refactor can inject `now` without
/// touching anything else.
function formatTime(d: Date | null): string {
  if (d === null) return "";
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  if (sameDay) return `${hh}:${mm}`;
  const mo = String(d.getMonth() + 1).padStart(2, "0");
  const da = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${mo}-${da} ${hh}:${mm}`;
}

export function UserBubble({ message, actions }: UserMessageProps) {
  const menu = useMessageMenu({
    id: message.id,
    text: message.text,
    actions,
    hasMeta: !!message.meta,
  });
  return (
    <div
      className="lutin-chat__msg lutin-chat__msg--user"
      onContextMenu={menu.onContextMenu}
      {...menu.dataAttrs}
    >
      {message.meta && (
        <div className="lutin-chat__msg-meta">
          <MetricsHeader meta={message.meta} />
        </div>
      )}
      {menu.editing ? (
        menu.editor
      ) : (
        <div className="lutin-chat__bubble lutin-chat__bubble--user">{message.text}</div>
      )}
      {menu.menu}
    </div>
  );
}

export function AssistantBubble({ message, actions }: AssistantMessageProps) {
  const streaming = !!message.streaming;
  const visible = useTypewriter(message.text, streaming);
  // Only attach actions to settled (non-streaming) assistant messages —
  // editing/deleting an in-flight buffer would race the agent.
  const menu = useMessageMenu({
    id: message.id,
    text: message.text,
    actions: streaming ? undefined : actions,
    hasMeta: !!message.meta,
  });

  // Split at the last newline: the prefix is committed and parsed as
  // markdown; the suffix is the in-flight line, rendered as plain text
  // with the cursor next to it. Avoids markdown jitter from half-open
  // emphasis/code spans, and keeps the cursor inline with the last char.
  let parsed = visible;
  let live = "";
  if (streaming) {
    const nl = visible.lastIndexOf("\n");
    if (nl < 0) {
      parsed = "";
      live = visible;
    } else {
      parsed = visible.slice(0, nl + 1);
      live = visible.slice(nl + 1);
    }
  }

  return (
    <article
      className="lutin-chat__msg lutin-chat__msg--assistant"
      onContextMenu={menu.onContextMenu}
      {...menu.dataAttrs}
    >
      {(streaming || message.meta) && (
        <div className="lutin-chat__msg-meta">
          {streaming && (
            <span className="lutin-chat__msg-status" aria-live="polite">streaming</span>
          )}
          {!streaming && <MetricsHeader meta={message.meta} />}
        </div>
      )}
      <div className="lutin-chat__msg-body">
        {menu.editing ? (
          menu.editor
        ) : (
          <>
            {parsed && <Markdown text={parsed} />}
            {streaming && (
              <p className="lutin-chat__msg-live">
                {live}
                <span className="lutin-chat__cursor" aria-hidden />
              </p>
            )}
          </>
        )}
      </div>
      {menu.menu}
    </article>
  );
}

export function AgentBubble({ message, actions }: AgentMessageProps) {
  const [open, setOpen] = useState(false);
  const menu = useMessageMenu({
    id: message.id,
    text: message.text,
    actions,
    hasMeta: !!message.meta,
  });
  const cls = [
    "lutin-chat__msg",
    "lutin-chat__msg--agent",
    !message.ok && "lutin-chat__msg--agent-failed",
  ]
    .filter(Boolean)
    .join(" ");
  // First non-empty line, used as the collapsed-state preview so the
  // user can scan agent replies without expanding each one.
  const preview = message.text.split("\n").find((l) => l.trim().length > 0) ?? "";
  return (
    <details
      className={cls}
      open={open}
      onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}
      onContextMenu={menu.onContextMenu}
      {...menu.dataAttrs}
    >
      <summary className="lutin-chat__msg-meta lutin-chat__agent-head">
        <span className="lutin-chat__who">
          <span className="lutin-chat__role">{message.agentId}</span>
          {!message.ok && (
            <span className="lutin-chat__msg-status" data-state="failed">
              failed
            </span>
          )}
        </span>
        {!open && preview && (
          <span className="lutin-chat__agent-preview">{preview}</span>
        )}
        <MetricsHeader meta={message.meta} />
        <span className="lutin-chat__thinking-toggle" aria-hidden>
          {open ? "−" : "+"}
        </span>
      </summary>
      <div className="lutin-chat__msg-body">
        {menu.editing ? menu.editor : <Markdown text={message.text} />}
      </div>
      {menu.menu}
    </details>
  );
}

export function SystemBubble({ message, actions }: SystemMessageProps) {
  const menu = useMessageMenu({
    id: message.id,
    text: message.text,
    actions,
    hasMeta: !!message.meta,
  });
  return (
    <div
      className="lutin-chat__msg lutin-chat__msg--system"
      onContextMenu={menu.onContextMenu}
      {...menu.dataAttrs}
    >
      {message.meta && (
        <div className="lutin-chat__msg-meta">
          <span className="lutin-chat__msg-role">system</span>
          <MetricsHeader meta={message.meta} />
        </div>
      )}
      {menu.editing ? (
        menu.editor
      ) : (
        <div className="lutin-chat__bubble lutin-chat__bubble--system">{message.text}</div>
      )}
      {menu.menu}
    </div>
  );
}

// Reveals `target` one character at a time when `enabled`. Adaptive:
// base 80 chars/sec, but scales up with the lag so we always catch up
// within a few frames if the upstream burst is large. When `enabled`
// flips off (turn ended) we snap to the full target so the final
// character isn't held back.
function useTypewriter(target: string, enabled: boolean): string {
  const [shown, setShown] = useState<string>(enabled ? "" : target);
  const targetRef = useRef(target);
  targetRef.current = target;

  // Reset the typewriter if the target string is replaced wholesale
  // (e.g. a new streaming message starts with a different prefix).
  useEffect(() => {
    setShown((prev) => (target.startsWith(prev) ? prev : ""));
  }, [target]);

  useEffect(() => {
    if (!enabled) {
      setShown(targetRef.current);
      return;
    }
    let raf = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const dt = now - last;
      last = now;
      setShown((prev) => {
        const t = targetRef.current;
        if (prev.length >= t.length) return prev === t ? prev : t;
        const gap = t.length - prev.length;
        const cps = Math.max(80, gap * 6);
        const advance = Math.max(1, Math.round((cps * dt) / 1000));
        return t.slice(0, Math.min(t.length, prev.length + advance));
      });
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [enabled]);

  return shown;
}
