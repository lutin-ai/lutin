// Projects the reviewed workflow's wire events onto the shared
// chat-widgets `ChatMessage` model so the transcript renders through
// `<ChatView>` with the same tool widgets (Edit diffs, file previews,
// Shell output) as the other workflows. Principle verdicts ride
// alongside in `verdictsByCallId`, grafted back onto each tool bubble by
// the ToolCall slot — mirroring `principled`'s session/adapter split.
//
// One tool bubble per (step, attempt): the engine drafts a call, reviews
// it, and on a blocking verdict rewinds and re-drafts under the same
// `stepId`. Each attempt is its own bubble (rejected drafts go `failed`,
// the accepted one goes `completed`), grouped by an active-step outline
// while the step is still under review.

import type { ChatEvent, ReviewVerdict, Turn } from "@lutin/reviewed-protocol";
import type { ChatMessage, TurnState } from "@lutin/chat-widgets";

export type Check =
  | { kind: "pass"; principle: string }
  | { kind: "fail"; principle: string; feedback: string }
  | { kind: "skipped"; principle: string; streak: number };

export interface Snapshot {
  persona: string | null;
  // Bumped once per user turn. `stepId` restarts at 0 each turn, so it
  // alone can't key bubbles across a session — `turnSeq:stepId:attempt`
  // does.
  turnSeq: number;
  messages: ChatMessage[];
  turn: TurnState;
  // Principle checks scored against a given attempt's `callId`, in
  // arrival order. The ToolCall slot reads this to render the panel.
  verdictsByCallId: Record<string, Check[]>;
  // `callId` → `turnSeq:stepId`, so the slot can outline every attempt
  // bubble belonging to a still-active step.
  stepIdByCallId: Record<string, string>;
  // Steps drafted but not yet executed — drives the active outline.
  activeStepKeys: string[];
  // `turnSeq:stepId` → latest attempt's `callId`; the accepted attempt
  // `toolCallExecuted` lands on is whichever drafted last.
  lastCallIdByStep: Record<string, string>;
}

export const initialSnapshot: Snapshot = {
  persona: null,
  turnSeq: 0,
  messages: [],
  turn: { kind: "idle" },
  verdictsByCallId: {},
  stepIdByCallId: {},
  activeStepKeys: [],
  lastCallIdByStep: {},
};

export type Action =
  | { type: "event"; event: ChatEvent }
  | { type: "loaded"; persona: string | null; turns: Turn[] }
  | { type: "optimistic"; text: string }
  | { type: "submitFailed"; message: string };

export function reduce(s: Snapshot, a: Action): Snapshot {
  switch (a.type) {
    case "loaded":
      return { ...s, persona: a.persona, messages: a.turns.map(turnToMessage) };
    case "optimistic": {
      const turnSeq = s.turnSeq + 1;
      return {
        ...s,
        turnSeq,
        messages: [...s.messages, { kind: "user", id: `u${turnSeq}`, text: a.text }],
        turn: { kind: "streaming" },
      };
    }
    case "submitFailed":
      return { ...s, turn: { kind: "errored", message: a.message } };
    case "event":
      return applyEvent(s, a.event);
  }
}

function turnToMessage(t: Turn): ChatMessage {
  switch (t.kind) {
    case "user":
      return { kind: "user", id: t.id, text: t.text };
    case "assistant":
      return { kind: "assistant", id: t.id, text: t.text };
    case "toolCall":
      return {
        kind: "toolCall",
        id: t.id,
        name: t.tool,
        args: { kind: "parsed", value: t.args },
        result: t.output,
        state: "completed",
      };
  }
}

function patchTool(
  messages: ChatMessage[],
  id: string,
  patch: Partial<Extract<ChatMessage, { kind: "toolCall" }>>,
): ChatMessage[] {
  return messages.map((m) =>
    m.kind === "toolCall" && m.id === id ? { ...m, ...patch } : m,
  );
}

function applyEvent(s: Snapshot, ev: ChatEvent): Snapshot {
  switch (ev.kind) {
    case "userMessageAppended": {
      // The engine echoes the message we already showed optimistically;
      // swallow the echo so it isn't doubled (and don't bump turnSeq).
      if (s.messages.some((m) => m.kind === "user" && m.text === ev.text)) return s;
      const turnSeq = s.turnSeq + 1;
      return {
        ...s,
        turnSeq,
        messages: [...s.messages, { kind: "user", id: ev.id, text: ev.text }],
        turn: { kind: "streaming" },
      };
    }
    case "assistantMessage":
      return {
        ...s,
        messages: [...s.messages, { kind: "assistant", id: ev.id, text: ev.text }],
      };
    case "thinking":
      return {
        ...s,
        messages: [
          ...s.messages,
          {
            kind: "thinking",
            id: `th-${s.turnSeq}-${ev.stepId}-${ev.attempt}-${s.messages.length}`,
            text: ev.text,
          },
        ],
      };
    case "toolCallDrafted": {
      const callId = `${s.turnSeq}:${ev.stepId}:${ev.attempt}`;
      const stepKey = `${s.turnSeq}:${ev.stepId}`;
      return {
        ...s,
        messages: [
          ...s.messages,
          {
            kind: "toolCall",
            id: callId,
            name: ev.tool,
            args: { kind: "parsed", value: ev.args },
            state: "running",
          },
        ],
        stepIdByCallId: { ...s.stepIdByCallId, [callId]: stepKey },
        lastCallIdByStep: { ...s.lastCallIdByStep, [stepKey]: callId },
        activeStepKeys: s.activeStepKeys.includes(stepKey)
          ? s.activeStepKeys
          : [...s.activeStepKeys, stepKey],
      };
    }
    case "principleEvaluated": {
      const callId = `${s.turnSeq}:${ev.stepId}:${ev.attempt}`;
      const check: Check =
        ev.verdict.kind === "pass"
          ? { kind: "pass", principle: ev.principle }
          : { kind: "fail", principle: ev.principle, feedback: failFeedback(ev.verdict) };
      const verdictsByCallId = {
        ...s.verdictsByCallId,
        [callId]: [...(s.verdictsByCallId[callId] ?? []), check],
      };
      // A blocking verdict rejects this draft; the engine short-circuits
      // and re-drafts, so mark the bubble failed. Feedback lives in the
      // panel, not on the bubble, to avoid showing it twice.
      const messages =
        ev.verdict.kind === "fail"
          ? patchTool(s.messages, callId, { state: "failed" })
          : s.messages;
      return { ...s, messages, verdictsByCallId };
    }
    case "principleSkipped": {
      const callId = `${s.turnSeq}:${ev.stepId}:${ev.attempt}`;
      return {
        ...s,
        verdictsByCallId: {
          ...s.verdictsByCallId,
          [callId]: [
            ...(s.verdictsByCallId[callId] ?? []),
            { kind: "skipped", principle: ev.principle, streak: ev.streak },
          ],
        },
      };
    }
    case "toolCallExecuted": {
      const stepKey = `${s.turnSeq}:${ev.stepId}`;
      const callId = s.lastCallIdByStep[stepKey];
      const messages = callId
        ? patchTool(s.messages, callId, {
            state: "completed",
            result: ev.output,
            args: { kind: "parsed", value: ev.args },
          })
        : [
            ...s.messages,
            {
              kind: "toolCall" as const,
              id: `${stepKey}:exec`,
              name: ev.tool,
              args: { kind: "parsed" as const, value: ev.args },
              result: ev.output,
              state: "completed" as const,
            },
          ];
      return {
        ...s,
        messages,
        activeStepKeys: s.activeStepKeys.filter((k) => k !== stepKey),
      };
    }
    case "turnFinished":
      return {
        ...s,
        activeStepKeys: [],
        turn:
          ev.reason.kind === "failed"
            ? { kind: "errored", message: ev.reason.message }
            : { kind: "idle" },
      };
    case "stateChanged":
      return { ...s, persona: ev.state.persona };
    default:
      return s;
  }
}

function failFeedback(v: ReviewVerdict): string {
  return v.kind === "fail" ? v.feedback : "";
}
