import { useEffect, useRef, useState } from "react";
import { Markdown } from "../markdown";
import type {
  AssistantMessageProps,
  SystemMessageProps,
  UserMessageProps,
} from "../slots";

export function UserBubble({ message }: UserMessageProps) {
  return (
    <div className="lutin-chat__msg lutin-chat__msg--user">
      <span className="lutin-chat__msg-role">you</span>
      <div className="lutin-chat__bubble lutin-chat__bubble--user">{message.text}</div>
    </div>
  );
}

export function AssistantBubble({ message }: AssistantMessageProps) {
  const streaming = !!message.streaming;
  const visible = useTypewriter(message.text, streaming);

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
    <article className="lutin-chat__msg lutin-chat__msg--assistant">
      <div className="lutin-chat__msg-meta">
        <span className="lutin-chat__who">
          <span className="lutin-chat__role">assistant</span>
        </span>
        {streaming && (
          <span className="lutin-chat__msg-status" aria-live="polite">streaming</span>
        )}
      </div>
      <div className="lutin-chat__msg-body">
        {parsed && <Markdown text={parsed} />}
        {streaming && (
          <p className="lutin-chat__msg-live">
            {live}
            <span className="lutin-chat__cursor" aria-hidden />
          </p>
        )}
      </div>
    </article>
  );
}

export function SystemBubble({ message }: SystemMessageProps) {
  return (
    <div className="lutin-chat__msg lutin-chat__msg--system">
      <div className="lutin-chat__bubble lutin-chat__bubble--system">{message.text}</div>
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
