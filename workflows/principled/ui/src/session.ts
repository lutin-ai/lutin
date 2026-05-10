// Session state reducer mirroring `workflows/chat/src/ui.rs::apply_chat_event` /
// `apply_chat_response`. Pure: takes the previous state and an action,
// returns the next state. The React component owns the dispatch.

import {
  type ChatError,
  type ChatEvent,
  type ChatResponse,
  type HistoricalMessage,
  type MessageMeta,
  type ReviewLogEntry,
  type ReviewVerdict,
  type SubAgentInfo,
  type ToolOutcome,
  chatErrorMessage,
} from "@lutin/principled-protocol";

/**
 * One tool exchange's lifecycle. Three states, encoded as variants so
 * `state.kind === "running"` and `state.kind === "ok"` carry different
 * payload shapes — no empty-string sentinels for "no result yet".
 */
export type ToolStatus =
  | { kind: "running" }
  | { kind: "ok"; output: string }
  | { kind: "failed"; reason: string };

/** Tool-call arguments lifecycle: either still streaming raw JSON
 *  fragments from the model, or fully parsed at the wire boundary.
 *  Encoded as a tagged union so the two states are mutually exclusive
 *  by construction. */
export type ToolArgs =
  | { kind: "streaming"; raw: string }
  | { kind: "parsed"; value: unknown };

export interface ToolEntry {
  role: "tool";
  /** Engine-assigned tool-call id; matches a started→completed pair. */
  callId: string;
  name: string;
  args: ToolArgs;
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
  | { kind: "errored"; message: string }
  | { kind: "maxRounds" };

/** Live state of a single in-flight review frame. Built from the
 *  engine's `reviewFrame*` / `reviewer*` events; dropped on
 *  `reviewFrameResolved`. The chat surface uses this to render a
 *  transient "reviewing …" placeholder where the tool call will
 *  eventually land; the sidebar uses the rolling verdict map for the
 *  expand-to-detail view of an active step. */
export interface ActiveReview {
  stepId: string;
  toolName: string;
  argsSummary: string;
  /** `null` while the frame is still on its first attempt — retry
   *  budgets are per-principle, so the engine can't honestly emit a
   *  pair on `ReviewFrameOpened`. The first `ReviewFrameProgress`
   *  carries `{ attempt, maxAttempts }` for the principle that
   *  actually failed; until then the chip renders "attempt 1" with
   *  no denominator. */
  progress: { attempt: number; maxAttempts: number } | null;
  /** Principle names currently blocking acceptance — drives the
   *  "N principles blocking" chip count. */
  blocking: string[];
  /** Latest verdict per principle for this attempt. Replaced when a
   *  later iteration produces a fresh verdict for the same principle. */
  verdicts: Record<string, ReviewVerdict>;
}

export interface SessionSnapshot {
  persona: string | null;
  /** Live review frames in open-order. Most sessions have at most
   *  one active frame at a time (only one tool call is ever in
   *  review), so a linear scan to find by `stepId` is cheap and
   *  preserves insertion order — `Object.keys` on a numeric-keyed
   *  Record sorts keys numerically and breaks ordering. */
  activeReviews: ActiveReview[];
  /** Persisted reviewer audit log. Replaced wholesale when the
   *  `reviews` ChatOk lands (after Subscribe), then appended live as
   *  each `reviewerCompleted` event fires. Sidebar reads this; it's
   *  never written to the chat scrollback. */
  reviewLog: ReviewLogEntry[];
  completed: Message[];
  /** Parallel to `completed` — the engine's `MetricsReplaced` broadcast
   *  always arrives paired with `HistoryReplaced`, so once both have
   *  been applied the arrays are aligned 1:1. While one has just landed
   *  but not the other, the lengths can diverge for a few ms — render
   *  defensively (treat missing entries as "no metrics yet"). */
  metrics: MessageMeta[];
  /** Live sub-agent registry rows. Replaced wholesale on each
   *  `subAgentsChanged` event (and on the response to `listSubAgents`). */
  subAgents: SubAgentInfo[];
  /** Per-id transcripts for sub-agents the user has opened. Keyed by
   *  `agent#N`; populated by `getSubAgentTranscript` responses and
   *  refreshed by `subAgentTranscriptUpdated` broadcasts. The reducer
   *  doesn't prune — sub-agent counts stay small per session. */
  subAgentTranscripts: Record<string, Message[]>;
  /** Live token counters projected from the latest `summaryUpdated`
   *  broadcast. Updated on every provider Usage report so the
   *  composer footer ticks during a long agent loop, not only at
   *  message-end. `null` until the first usage report arrives. */
  summary: {
    contextTokens: number | null;
    totalPromptTokens: number;
    totalCompletionTokens: number;
  } | null;
  /** Per-tool-bubble verdict history, keyed by the attempt's `callId`.
   *  Populated from `reviewerCompleted` events; the inline panel under
   *  each tool bubble reads this list to render filtered reviewer rows
   *  (default: hide pure pass, show fail / pass-with-nit). Survives
   *  squashes — the engine drops the matching tool entry and the UI
   *  no longer renders the panel, so the lingering verdicts in the
   *  map are harmless and avoid a coupled cleanup pass. */
  verdictsByCallId: Record<string, ReviewLogEntry[]>;
  /** `callId` → `stepId.toString()`. Populated when a `reviewFrameOpened`
   *  / `reviewerStarted` lands, so the iteration-box outline can group
   *  every tool bubble that belongs to the active step (multiple Fix
   *  retries within one step share a stepId but get distinct callIds). */
  stepIdByCallId: Record<string, string>;
  turn: Turn;
}

export const initialSnapshot: SessionSnapshot = {
  persona: null,
  activeReviews: [],
  reviewLog: [],
  completed: [],
  metrics: [],
  subAgents: [],
  subAgentTranscripts: {},
  summary: null,
  verdictsByCallId: {},
  stepIdByCallId: {},
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
    case "toolCallStreaming":
      return pushToolStart(state, ev.id, ev.name);
    case "toolCallArgsDelta":
      return appendToolArgs(state, ev.id, ev.args);
    case "toolCallArgsParsed":
      return setToolArgsParsed(state, ev.id, ev.name, ev.arguments);
    case "toolCallCompleted":
      return updateToolCompleted(state, ev.id, ev.outcome);
    case "messageFinished": {
      const completed = flushStreaming(state);
      const turn: Turn =
        ev.reason.kind === "failed"
          ? { kind: "errored", message: ev.reason.message }
          : ev.reason.kind === "maxRounds"
            ? { kind: "maxRounds" }
            : { kind: "idle" };
      return { ...state, completed, turn };
    }
    case "stateChanged":
      return {
        ...state,
        persona: ev.state.persona,
      };
    case "historyReplaced": {
      const completed: Message[] = ev.history.map(fromHistorical);
      // A historyReplaced mid-stream means the engine rewrote the
      // canonical transcript out from under us — most commonly a
      // reviewer-driven rewind, which cancels the agent without
      // emitting `toolCallCompleted` for the in-flight tool. Anything
      // accumulated in `turn.flushed` / `turn.buf` predates the
      // rewrite: pre-rewind tool entries would otherwise stay stuck
      // on `running` forever, and pre-rewind text would be re-glued
      // into `completed` on the next `messageFinished`. Drop both —
      // post-rewind stream events repopulate `flushed` cleanly.
      if (state.turn.kind === "streaming") {
        return {
          ...state,
          completed,
          turn: { kind: "streaming", stream: state.turn.stream, buf: "", flushed: [] },
        };
      }
      return { ...state, completed };
    }
    case "metricsReplaced":
      return { ...state, metrics: ev.metrics };
    case "subAgentsChanged":
      return { ...state, subAgents: ev.subAgents };
    case "subAgentTranscriptUpdated":
      return {
        ...state,
        subAgentTranscripts: {
          ...state.subAgentTranscripts,
          [ev.id]: ev.history.map(fromHistorical),
        },
      };
    case "reviewFrameOpened": {
      const stepId = ev.stepId.toString();
      const frame: ActiveReview = {
        stepId,
        toolName: ev.toolName,
        argsSummary: ev.argsSummary,
        progress: null,
        blocking: [],
        verdicts: {},
      };
      return {
        ...state,
        activeReviews: [...state.activeReviews, frame],
        stepIdByCallId: { ...state.stepIdByCallId, [ev.callId]: stepId },
      };
    }
    case "reviewerStarted":
      // Tag the in-flight attempt's bubble with its stepId so the
      // iteration-box outline picks it up immediately — even on the
      // first reviewer's started event, before any verdict has landed.
      // No verdicts yet, no `verdictsByCallId` write here.
      return {
        ...state,
        stepIdByCallId: {
          ...state.stepIdByCallId,
          [ev.callId]: ev.stepId.toString(),
        },
      };
    case "reviewerCompleted": {
      const stepId = ev.stepId.toString();
      const frame = state.activeReviews.find((f) => f.stepId === stepId);
      const logEntry: ReviewLogEntry = {
        ts: ev.ts,
        stepId: ev.stepId,
        reviewerCallId: ev.reviewerCallId,
        principle: ev.principle,
        // Persona join is denormalized into `reviews.jsonl` but not
        // into the live event (saves a hashmap lookup engine-side).
        // `null` here means "look it up from the principle list" —
        // the sidebar resolves it client-side at render time.
        persona: null,
        toolName: frame?.toolName ?? "",
        argsSummary: frame?.argsSummary ?? "",
        verdict: ev.verdict,
        callId: ev.callId,
      };
      const reviewLog = [...state.reviewLog, logEntry];
      const priorForCall = state.verdictsByCallId[ev.callId] ?? [];
      const verdictsByCallId = {
        ...state.verdictsByCallId,
        [ev.callId]: [...priorForCall, logEntry],
      };
      if (!frame) {
        return { ...state, reviewLog, verdictsByCallId };
      }
      const activeReviews = state.activeReviews.map((f) =>
        f.stepId === stepId
          ? { ...f, verdicts: { ...f.verdicts, [ev.principle]: ev.verdict } }
          : f,
      );
      return { ...state, activeReviews, reviewLog, verdictsByCallId };
    }
    case "reviewFrameProgress": {
      const stepId = ev.stepId.toString();
      const activeReviews = state.activeReviews.map((f) =>
        f.stepId === stepId
          ? {
              ...f,
              progress: { attempt: ev.attempt, maxAttempts: ev.maxAttempts },
              blocking: ev.blocking,
              // A new attempt invalidates the prior round's verdict
              // map — every reviewer re-runs, so stale "pass"
              // entries must not stick to the chip.
              verdicts: {},
            }
          : f,
      );
      return { ...state, activeReviews };
    }
    case "reviewFrameResolved": {
      const stepId = ev.stepId.toString();
      const activeReviews = state.activeReviews.filter(
        (f) => f.stepId !== stepId,
      );
      if (activeReviews.length === state.activeReviews.length) return state;
      return { ...state, activeReviews };
    }
    case "summaryUpdated":
      return {
        ...state,
        summary: {
          contextTokens: ev.contextTokens,
          totalPromptTokens: ev.totalPromptTokens,
          totalCompletionTokens: ev.totalCompletionTokens,
        },
      };
    case "attemptsSquashed": {
      // Live mirror of `engine::squash_denied_attempts`. The engine
      // emits this when a step transitions to a terminal status, so
      // the UI sheds the failed-attempt bubbles immediately rather
      // than waiting for end-of-turn `historyReplaced`. The accepted
      // (or rewound-into) attempt's callId is intentionally excluded
      // by the engine — only `DeniedRetry`/`Rewound` outcomes get
      // squashed, matching the projection in `project_messages`.
      const drop = new Set(ev.callIds);
      if (drop.size === 0) return state;
      const filterFn = (m: Message) =>
        m.role !== "tool" || !drop.has(m.callId);
      const completed = state.completed.filter(filterFn);
      if (state.turn.kind === "streaming") {
        const flushed = state.turn.flushed.filter(filterFn);
        return { ...state, completed, turn: { ...state.turn, flushed } };
      }
      return { ...state, completed };
    }
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
): SessionSnapshot {
  const entry: ToolEntry = {
    role: "tool",
    callId,
    name,
    args: { kind: "streaming", raw: "" },
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

// Append a streaming-args fragment to the running ToolEntry for `callId`.
// Once `args.kind === "parsed"`, deltas are ignored — the parsed
// value is the source of truth and shouldn't be shadowed by a tail
// fragment.
function appendToolArgs(
  state: SessionSnapshot,
  callId: string,
  args: string,
): SessionSnapshot {
  const updateOne = (m: Message): Message => {
    if (m.role !== "tool" || m.callId !== callId) return m;
    if (m.args.kind !== "streaming") return m;
    return { ...m, args: { kind: "streaming", raw: m.args.raw + args } };
  };
  if (state.turn.kind === "streaming") {
    return {
      ...state,
      turn: { ...state.turn, flushed: state.turn.flushed.map(updateOne) },
    };
  }
  return { ...state, completed: state.completed.map(updateOne) };
}

// All argument fragments arrived: swap the streaming `args` for the
// parsed value. Status stays `running` — the actual tool dispatch
// fires next, completion lands via toolCallCompleted. The agent SDK
// guarantees a `toolCallStreaming` precedes every `toolCallArgsParsed`
// for a given id, so a missing entry would be a producer bug; we let
// it disappear silently rather than synthesize a stand-in here.
function setToolArgsParsed(
  state: SessionSnapshot,
  callId: string,
  name: string,
  value: unknown,
): SessionSnapshot {
  const updateOne = (m: Message): Message => {
    if (m.role !== "tool" || m.callId !== callId) return m;
    return { ...m, name, args: { kind: "parsed", value } };
  };
  if (state.turn.kind === "streaming") {
    return {
      ...state,
      turn: { ...state.turn, flushed: state.turn.flushed.map(updateOne) },
    };
  }
  return { ...state, completed: state.completed.map(updateOne) };
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
        args: { kind: "parsed", value: h.arguments },
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
      return {
        ...state,
        persona: ok.state.persona,
        completed,
      };
    }
    case "state":
    case "stateUpdated":
      return {
        ...state,
        persona: ok.state.persona,
      };
    case "metrics":
      return { ...state, metrics: ok.metrics };
    case "subAgents":
      return { ...state, subAgents: ok.subAgents };
    case "subAgentTranscript":
      return {
        ...state,
        subAgentTranscripts: {
          ...state.subAgentTranscripts,
          [ok.id]: ok.history.map(fromHistorical),
        },
      };
    case "reviews": {
      // Engine-side replay of `reviews.jsonl`, used to hydrate a
      // late-joining client. The Subscribe response races
      // ListReviews — live `reviewerCompleted` events that land
      // between the two would be wiped by a wholesale replace.
      // Merge by `(stepId, reviewerCallId)` so replay seeds the log
      // and any rows already appended via the live broadcast stay
      // put. Reviewer-call ids are unique per session, so the dedupe
      // key is exact.
      const seen = new Set(
        state.reviewLog.map((r) => `${r.stepId}:${r.reviewerCallId}`),
      );
      const merged = [...state.reviewLog];
      for (const row of ok.reviews) {
        const key = `${row.stepId}:${row.reviewerCallId}`;
        if (seen.has(key)) continue;
        seen.add(key);
        merged.push(row);
      }
      // Replay is in wall-clock order on disk; live rows are too.
      // After merge the list may interleave depending on arrival
      // order, but the sidebar sorts visually by ts on render if it
      // ever needs to — for v1 the natural append order is good
      // enough since both sources are monotonic.
      return { ...state, reviewLog: merged };
    }
    case "messageQueued":
    case "cancelled":
    case "personas":
    case "historyAcknowledged":
      // Personas are fetched by App.tsx separately and
      // never flow through this reducer. historyAcknowledged is a
      // no-op on purpose — the engine delivers the new transcript
      // via the `historyReplaced` broadcast (see applyEvent), so
      // applying it here too would be redundant. These cases exist
      // to keep the exhaustiveness check honest.
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
