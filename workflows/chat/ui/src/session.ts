// Session state reducer mirroring `workflows/chat/src/ui.rs::apply_chat_event` /
// `apply_chat_response`. Pure: takes the previous state and an action,
// returns the next state. The React component owns the dispatch.

import {
  type ChatError,
  type ChatEvent,
  type ChatResponse,
  type HistoricalMessage,
  chatErrorMessage,
} from "./chat";

export type Role = "user" | "assistant" | "thinking";

export interface Message {
  role: Role;
  text: string;
}

export type StreamKind = "assistant" | "thinking";

export type Turn =
  | { kind: "idle" }
  | { kind: "streaming"; stream: StreamKind; buf: string; flushed: Message[] }
  | { kind: "errored"; message: string };

export interface SessionSnapshot {
  persona: string | null;
  completed: Message[];
  turn: Turn;
}

export const initialSnapshot: SessionSnapshot = {
  persona: null,
  completed: [],
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
    case "toolCallCompleted":
      return state;
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
      ? [...state.turn.flushed, { role: state.turn.stream, text: state.turn.buf }]
      : state.turn.flushed;
  return {
    ...state,
    turn: { kind: "streaming", stream: kind, buf: text, flushed },
  };
}

function applyResponse(
  state: SessionSnapshot,
  resp: ChatResponse,
): SessionSnapshot {
  if (!resp.ok) return applyError(state, resp.error);
  const ok = resp.value;
  switch (ok.kind) {
    case "subscribed": {
      const completed: Message[] = ok.history.map(
        (h: HistoricalMessage): Message => ({ role: h.role, text: h.text }),
      );
      return { ...state, persona: ok.state.persona, completed };
    }
    case "state":
    case "stateUpdated":
      return { ...state, persona: ok.state.persona };
    case "messageQueued":
    case "cancelled":
    case "personas":
      // Personas is fetched by App.tsx separately and never flows
      // through this reducer; the case is here only to keep the
      // exhaustiveness check happy.
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
  const out = [...state.completed, ...state.turn.flushed];
  if (state.turn.buf.length > 0) {
    out.push({ role: state.turn.stream, text: state.turn.buf });
  }
  return out;
}
