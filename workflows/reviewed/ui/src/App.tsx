// Minimal append-only chat for the reviewed workflow. No persistence
// replay, no persona picker — every event from the engine appends a
// new entry to a flat log. Good enough to watch the loop work; the
// scratchpad UI is the template if this needs to grow.

import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import {
  type ChatEvent,
  type ReviewVerdict,
  decodeChatEvent,
  encodeChatRequest,
} from "@lutin/reviewed-protocol";
import type { Lutin } from "./lutin";

type Entry =
  | { kind: "user"; id: string; text: string }
  | { kind: "assistant"; id: string; text: string }
  | {
      kind: "draft";
      id: string;
      step: bigint;
      attempt: number;
      tool: string;
      args: unknown;
    }
  | {
      kind: "review";
      id: string;
      step: bigint;
      attempt: number;
      principle: string;
      verdict: ReviewVerdict;
    }
  | {
      kind: "executed";
      id: string;
      step: bigint;
      tool: string;
      args: unknown;
      output: string;
    }
  | { kind: "system"; id: string; text: string };

interface State {
  log: Entry[];
  inFlight: boolean;
}

const initial: State = { log: [], inFlight: false };

type Action =
  | { type: "event"; event: ChatEvent }
  | { type: "optimistic"; text: string }
  | { type: "submitFailed"; message: string };

let counter = 0;
const newId = () => `e-${++counter}`;

function reduce(s: State, a: Action): State {
  switch (a.type) {
    case "optimistic":
      return {
        ...s,
        log: [...s.log, { kind: "user", id: newId(), text: a.text }],
        inFlight: true,
      };
    case "submitFailed":
      return {
        ...s,
        log: [...s.log, { kind: "system", id: newId(), text: a.message }],
        inFlight: false,
      };
    case "event":
      return applyEvent(s, a.event);
  }
}

function applyEvent(s: State, ev: ChatEvent): State {
  switch (ev.kind) {
    case "userMessageAppended":
      // Engine echoes the user message; the optimistic entry already
      // shows it, so swallow the echo if we have a pending one.
      if (s.log.some((e) => e.kind === "user" && e.text === ev.text)) return s;
      return {
        ...s,
        log: [...s.log, { kind: "user", id: ev.id, text: ev.text }],
      };
    case "assistantMessage":
      return {
        ...s,
        log: [...s.log, { kind: "assistant", id: ev.id, text: ev.text }],
      };
    case "toolCallDrafted":
      return {
        ...s,
        log: [
          ...s.log,
          {
            kind: "draft",
            id: newId(),
            step: ev.stepId,
            attempt: ev.attempt,
            tool: ev.tool,
            args: ev.args,
          },
        ],
      };
    case "principleEvaluated":
      return {
        ...s,
        log: [
          ...s.log,
          {
            kind: "review",
            id: newId(),
            step: ev.stepId,
            attempt: ev.attempt,
            principle: ev.principle,
            verdict: ev.verdict,
          },
        ],
      };
    case "toolCallExecuted":
      return {
        ...s,
        log: [
          ...s.log,
          {
            kind: "executed",
            id: newId(),
            step: ev.stepId,
            tool: ev.tool,
            args: ev.args,
            output: ev.output,
          },
        ],
      };
    case "turnFinished": {
      const note =
        ev.reason.kind === "completed"
          ? "(turn completed)"
          : ev.reason.kind === "cancelled"
            ? "(turn cancelled)"
            : `(turn failed: ${ev.reason.message})`;
      return {
        ...s,
        log: [...s.log, { kind: "system", id: newId(), text: note }],
        inFlight: false,
      };
    }
    case "stateChanged":
      return s;
    default:
      return s;
  }
}

interface Props {
  lutin: Lutin;
}

export function App({ lutin }: Props) {
  const [state, dispatch] = useReducer(reduce, initial);
  const [draft, setDraft] = useState("");
  const transcriptRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);

  const onTranscriptScroll = useCallback(() => {
    const el = transcriptRef.current;
    if (!el) return;
    pinnedToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 32;
  }, []);

  useEffect(() => {
    if (!pinnedToBottom.current) return;
    transcriptRef.current?.scrollTo({ top: transcriptRef.current.scrollHeight });
  }, [state.log]);

  useEffect(() => {
    const off = lutin.onBroadcast((body) => {
      try {
        dispatch({ type: "event", event: decodeChatEvent(body) });
      } catch (err) {
        console.warn("malformed ChatEvent broadcast", err);
      }
    });
    // We don't replay history; just subscribe so the engine starts
    // forwarding broadcasts to us. The response payload is discarded.
    lutin
      .request(encodeChatRequest({ kind: "subscribe" }))
      .catch((err) => console.warn("subscribe failed", err));
    return off;
  }, [lutin]);

  const send = useCallback(
    (text: string) => {
      const t = text.trim();
      if (!t) return;
      dispatch({ type: "optimistic", text: t });
      setDraft("");
      lutin
        .request(encodeChatRequest({ kind: "sendMessage", text: t }))
        .catch((err) =>
          dispatch({ type: "submitFailed", message: `send: ${String(err)}` }),
        );
    },
    [lutin],
  );

  const cancel = useCallback(() => {
    lutin
      .request(encodeChatRequest({ kind: "cancel" }))
      .catch((err) => console.warn("cancel failed", err));
  }, [lutin]);

  return (
    <div className="app">
      <div className="transcript" ref={transcriptRef} onScroll={onTranscriptScroll}>
        <div className="transcript-inner">
          {state.log.map((e) => (
            <EntryView key={e.id} entry={e} />
          ))}
        </div>
      </div>
      <div className="composer">
        <textarea
          value={draft}
          onChange={(ev) => setDraft(ev.target.value)}
          onKeyDown={(ev) => {
            if (ev.key === "Enter" && !ev.shiftKey) {
              ev.preventDefault();
              send(draft);
            }
          }}
          placeholder={state.inFlight ? "Working…" : "Send a message (Enter)"}
          rows={3}
        />
        <div className="composer-actions">
          {state.inFlight ? (
            <button onClick={cancel}>Cancel</button>
          ) : (
            <button onClick={() => send(draft)} disabled={!draft.trim()}>
              Send
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

function EntryView({ entry }: { entry: Entry }) {
  switch (entry.kind) {
    case "user":
      return (
        <div className="row row--user">
          <div className="user-bubble">{entry.text}</div>
        </div>
      );
    case "assistant":
      return (
        <div className="row row--assistant">
          <div className="assistant-bubble">{entry.text}</div>
        </div>
      );
    case "draft":
      return (
        <div className="row row--note">
          <div className="note note--draft">
            <span className="tag">draft #{entry.attempt}</span>
            <code>{entry.tool}</code>
            <pre>{prettyJson(entry.args)}</pre>
          </div>
        </div>
      );
    case "review": {
      const v = entry.verdict;
      const cls = `note note--${v.kind}`;
      return (
        <div className="row row--note">
          <div className={cls}>
            <span className="tag">{v.kind}</span>
            <code>{entry.principle}</code>
            {v.kind !== "pass" && <div className="feedback">{v.feedback}</div>}
          </div>
        </div>
      );
    }
    case "executed":
      return (
        <div className="row row--note">
          <div className="note note--executed">
            <span className="tag">ran</span>
            <code>{entry.tool}</code>
            <pre>{prettyJson(entry.args)}</pre>
            <div className="output">{entry.output}</div>
          </div>
        </div>
      );
    case "system":
      return (
        <div className="row row--note">
          <div className="note note--system">{entry.text}</div>
        </div>
      );
  }
}

function prettyJson(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}
