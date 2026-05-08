// Session state reducer mirroring `workflows/chat/src/ui.rs::apply_chat_event` /
// `apply_chat_response`. Pure: takes the previous state and an action,
// returns the next state. The React component owns the dispatch.

import {
  type ChatError,
  type ChatEvent,
  type ChatResponse,
  type HistoricalMessage,
  type MessageMeta,
  type ToolOutcome,
  chatErrorMessage,
} from "./chat";

/**
 * One tool exchange's lifecycle. Three states, encoded as variants so
 * `state.kind === "running"` and `state.kind === "ok"` carry different
 * payload shapes — no empty-string sentinels for "no result yet".
 */
export type ToolStatus =
  | { kind: "running" }
  | { kind: "ok"; output: string }
  | { kind: "failed"; reason: string };

export interface ToolEntry {
  role: "tool";
  /** Engine-assigned tool-call id; matches a started→completed pair. */
  callId: string;
  name: string;
  /** Parsed JSON, decoded once at the wire boundary. */
  arguments: unknown;
  status: ToolStatus;
}

export type Message =
  | { role: "user"; text: string }
  | { role: "assistant"; text: string }
  | { role: "thinking"; text: string }
  | ToolEntry
  | { role: "subAgentReply"; agentId: string; text: string }
  | { role: "subAgentFailure"; agentId: string; reason: string };

export type StreamKind = "assistant" | "thinking";

export type Turn =
  | { kind: "idle" }
  | { kind: "streaming"; stream: StreamKind; buf: string; flushed: Message[] }
  | { kind: "errored"; message: string };

export interface SessionSnapshot {
  persona: string | null;
  completed: Message[];
  /** Parallel to `completed` — the engine's `MetricsReplaced` broadcast
   *  always arrives paired with `HistoryReplaced`, so once both have
   *  been applied the arrays are aligned 1:1. While one has just landed
   *  but not the other, the lengths can diverge for a few ms — render
   *  defensively (treat missing entries as "no metrics yet"). */
  metrics: MessageMeta[];
  turn: Turn;
}

export const initialSnapshot: SessionSnapshot = {
  persona: null,
  completed: [],
  metrics: [],
  turn: { kind: "idle" },
};

export type Action =
  | { type: "event"; event: ChatEvent }
  | { type: "response"; response: ChatResponse }
  | { type: "submitOptimistic"; text: string }
  | { type: "rerunOptimistic" }
  | { type: "submitFailed"; message: string };

export function reduce(state: SessionSnapshot, action: Action): SessionSnapshot {
  switch (action.type) {
    case "event":
      return applyEvent(state, action.event);
    case "response":
      return applyResponse(state, action.response);
    case "submitOptimistic":
      return {
        ...state,
        completed: [...state.completed, { role: "user", text: action.text }],
        turn: { kind: "streaming", stream: "assistant", buf: "", flushed: [] },
      };
    case "rerunOptimistic":
      return {
        ...state,
        turn: { kind: "streaming", stream: "assistant", buf: "", flushed: [] },
      };
    case "submitFailed":
      return {
        ...state,
        turn: { kind: "errored", message: action.message },
      };
  }
}

function applyEvent(state: SessionSnapshot, ev: ChatEvent): SessionSnapshot {
  switch (ev.kind) {
    case "delta":
      return appendStream(state, "assistant", ev.text);
    case "reasoning":
      return appendStream(state, "thinking", ev.text);
    case "toolCallStarted":
      return pushToolStart(state, ev.id, ev.name, ev.arguments);
    case "toolCallCompleted":
      return updateToolCompleted(state, ev.id, ev.outcome);
    case "messageFinished": {
      const completed = flushStreaming(state);
      const turn: Turn =
        ev.reason.kind === "failed"
          ? { kind: "errored", message: ev.reason.message }
          : { kind: "idle" };
      return { ...state, completed, turn };
    }
    case "stateChanged":
      return { ...state, persona: ev.state.persona };
    case "historyReplaced": {
      const completed: Message[] = ev.history.map(fromHistorical);
      return { ...state, completed };
    }
    case "metricsReplaced":
      return { ...state, metrics: ev.metrics };
  }
}

// Append delta text to the current streaming segment. If the active
// stream kind switches (e.g. thinking → assistant), the prior buf is
// finalized as a flushed segment so both blocks render side by side.
function appendStream(
  state: SessionSnapshot,
  kind: StreamKind,
  text: string,
): SessionSnapshot {
  if (state.turn.kind !== "streaming") {
    return {
      ...state,
      turn: { kind: "streaming", stream: kind, buf: text, flushed: [] },
    };
  }
  if (state.turn.stream === kind) {
    return {
      ...state,
      turn: { ...state.turn, buf: state.turn.buf + text },
    };
  }
  const flushed =
    state.turn.buf.length > 0
      ? [...state.turn.flushed, { role: state.turn.stream, text: state.turn.buf } as Message]
      : state.turn.flushed;
  return {
    ...state,
    turn: { kind: "streaming", stream: kind, buf: text, flushed },
  };
}

// A tool call begins: flush whatever text is in `buf` into `flushed`
// (so the tool block lands after the in-progress text in render order),
// then append the tool entry as a "running" segment. Subsequent text
// deltas open a fresh buf above this entry. When called from idle (no
// streaming turn), enter streaming with the tool entry pre-flushed —
// the engine sometimes emits ToolCallStarted before any AssistantText.
function pushToolStart(
  state: SessionSnapshot,
  callId: string,
  name: string,
  args: unknown,
): SessionSnapshot {
  const entry: ToolEntry = {
    role: "tool",
    callId,
    name,
    arguments: args,
    status: { kind: "running" },
  };
  if (state.turn.kind !== "streaming") {
    return {
      ...state,
      turn: { kind: "streaming", stream: "assistant", buf: "", flushed: [entry] },
    };
  }
  const prior = state.turn.flushed;
  const flushed: Message[] =
    state.turn.buf.length > 0
      ? [...prior, { role: state.turn.stream, text: state.turn.buf }, entry]
      : [...prior, entry];
  return {
    ...state,
    turn: { ...state.turn, buf: "", flushed },
  };
}

// First-match-wins; ids are unique per session.
function updateToolCompleted(
  state: SessionSnapshot,
  callId: string,
  outcome: ToolOutcome,
): SessionSnapshot {
  const status: ToolStatus =
    outcome.kind === "ok"
      ? { kind: "ok", output: outcome.text }
      : { kind: "failed", reason: outcome.text };
  const updateOne = (m: Message): Message => {
    if (m.role !== "tool" || m.callId !== callId) return m;
    return { ...m, status };
  };
  if (state.turn.kind === "streaming") {
    const flushed = state.turn.flushed.map(updateOne);
    return { ...state, turn: { ...state.turn, flushed } };
  }
  return { ...state, completed: state.completed.map(updateOne) };
}

function fromHistorical(h: HistoricalMessage): Message {
  switch (h.kind) {
    case "user":
      return { role: "user", text: h.text };
    case "assistant":
      return { role: "assistant", text: h.text };
    case "thinking":
      return { role: "thinking", text: h.text };
    case "subAgentReply":
      return { role: "subAgentReply", agentId: h.agentId, text: h.text };
    case "subAgentFailure":
      return { role: "subAgentFailure", agentId: h.agentId, reason: h.reason };
    case "tool": {
      const status: ToolStatus =
        h.outcome === null
          ? { kind: "running" }
          : h.outcome.kind === "ok"
            ? { kind: "ok", output: h.outcome.text }
            : { kind: "failed", reason: h.outcome.text };
      return {
        role: "tool",
        callId: h.callId,
        name: h.name,
        arguments: h.arguments,
        status,
      };
    }
  }
}

function applyResponse(
  state: SessionSnapshot,
  resp: ChatResponse,
): SessionSnapshot {
  if (!resp.ok) return applyError(state, resp.error);
  const ok = resp.value;
  switch (ok.kind) {
    case "subscribed": {
      const completed: Message[] = ok.history.map(fromHistorical);
      return { ...state, persona: ok.state.persona, completed };
    }
    case "state":
    case "stateUpdated":
      return { ...state, persona: ok.state.persona };
    case "metrics":
      return { ...state, metrics: ok.metrics };
    case "messageQueued":
    case "cancelled":
    case "personas":
    case "historyAcknowledged":
      // Personas is fetched by App.tsx separately and never flows
      // through this reducer. historyAcknowledged is a no-op on
      // purpose — the engine delivers the new transcript via the
      // `historyReplaced` broadcast (see applyEvent), so applying it
      // here too would be redundant. These cases exist to keep the
      // exhaustiveness check honest.
      return state;
  }
}

function applyError(state: SessionSnapshot, err: ChatError): SessionSnapshot {
  // Engine refused the request — drop any in-flight buf into completed
  // so partial text isn't lost behind the error banner.
  const completed = flushStreaming(state);
  return {
    ...state,
    completed,
    turn: { kind: "errored", message: chatErrorMessage(err) },
  };
}

function flushStreaming(state: SessionSnapshot): Message[] {
  if (state.turn.kind !== "streaming") return state.completed;
  const out: Message[] = [...state.completed, ...state.turn.flushed];
  if (state.turn.buf.length > 0) {
    out.push({ role: state.turn.stream, text: state.turn.buf });
  }
  return out;
}
