// Reducer that folds engine `ChatEvent`s into the local view-model
// `Turn[]` consumed by `App.tsx` / `StepCard.tsx`. The wire types stay
// camelCase (protocol mirror); the view-model keeps the snake_case
// shapes already wired into the rendering components, so this file is
// the single place that bridges the two casings.

import type {
  ChatError,
  ChatEvent,
  ChatOk,
  ChatResponse,
  FixEntry,
  PersistentFailure,
  PersonaInfo,
  Plan as WirePlan,
  SessionState,
  Step as WireStep,
  StepStatus as WireStepStatus,
  Turn as WireTurn,
  Verdict as WireVerdict,
} from "@lutin/scratchpad-protocol";
import type {
  IterationRecord,
  Plan,
  Step,
  StepStatus,
  Turn,
  Verdict,
} from "./types";

export interface Snapshot {
  state: SessionState;
  turns: Turn[];
  inFlight: boolean;
  error: string | null;
  personas: PersonaInfo[] | null;
}

export const initialSnapshot: Snapshot = {
  state: { persona: null },
  turns: [],
  inFlight: false,
  error: null,
  personas: null,
};

export type Action =
  | { type: "response"; response: ChatResponse }
  | { type: "event"; event: ChatEvent }
  | { type: "submitOptimistic"; text: string }
  | { type: "submitFailed"; message: string };

export function reduce(s: Snapshot, a: Action): Snapshot {
  switch (a.type) {
    case "submitOptimistic": {
      const turn: Turn = { kind: "user", id: optimisticId(), text: a.text };
      return { ...s, turns: [...s.turns, turn], inFlight: true, error: null };
    }
    case "submitFailed":
      return { ...s, inFlight: false, error: a.message };
    case "response":
      return applyResponse(s, a.response);
    case "event":
      return applyEvent(s, a.event);
    default:
      return assertNever(a);
  }
}

function applyResponse(s: Snapshot, resp: ChatResponse): Snapshot {
  if (!resp.ok) return { ...s, error: errorMessage(resp.error), inFlight: false };
  return applyOk(s, resp.value);
}

function applyOk(s: Snapshot, ok: ChatOk): Snapshot {
  switch (ok.kind) {
    case "subscribed":
      return {
        ...s,
        state: ok.state,
        turns: ok.turns.map(wireTurnToView),
      };
    case "messageQueued":
      return { ...s, inFlight: true };
    case "cancelled":
      return { ...s, inFlight: false };
    case "state":
    case "stateUpdated":
      return { ...s, state: ok.state };
    case "personas":
      return { ...s, personas: ok.personas };
    default:
      return assertNever(ok);
  }
}

function applyEvent(s: Snapshot, ev: ChatEvent): Snapshot {
  switch (ev.kind) {
    case "userMessageAppended": {
      // Engine echoes the user message back as a broadcast after the
      // optimistic local append. Replace the pending turn (same text)
      // with the engine-issued id rather than appending a duplicate.
      const idx = s.turns.findIndex(
        (t) => t.kind === "user" && t.id.startsWith("u-pending-") && t.text === ev.text,
      );
      if (idx !== -1) {
        const turns = s.turns.slice();
        turns[idx] = { kind: "user", id: ev.id, text: ev.text };
        return { ...s, turns };
      }
      return { ...s, turns: [...s.turns, { kind: "user", id: ev.id, text: ev.text }] };
    }
    case "assistantMessage":
      return { ...s, turns: [...s.turns, { kind: "assistant", id: ev.id, text: ev.text }] };
    case "stepStarted":
      return appendStep(s, ev.stepId, { stage: "plan" });
    case "planProposed":
      return updateStep(s, ev.stepId, (step) => ({
        ...step,
        status: { stage: "iterate", plan: wirePlanToView(ev.plan), fix_log: [] },
      }));
    case "planRethink":
      return updateStep(s, ev.stepId, (step) => ({ ...step, status: { stage: "plan" } }));
    case "iterationStarted":
      return updateStep(s, ev.stepId, (step) => {
        const it: IterationRecord = {
          index: ev.index,
          args: ev.args,
          principle_verdicts: [],
        };
        return { ...step, iterations: replaceOrAppendIteration(step.iterations, it) };
      });
    case "principleEvaluated":
      return updateStep(s, ev.stepId, (step) => {
        const verdict = wireVerdictToView(ev.verdict);
        const iterations = step.iterations.map((it) =>
          it.index === ev.iteration
            ? {
                ...it,
                principle_verdicts: [
                  ...it.principle_verdicts,
                  { principle: ev.principle, verdict },
                ],
              }
            : it,
        );
        return { ...step, iterations };
      });
    case "scratchpadEdited":
      return updateStep(s, ev.stepId, (step) => {
        if (step.iterations.length === 0) return step;
        const iterations = step.iterations.slice();
        iterations[iterations.length - 1] = {
          ...iterations[iterations.length - 1],
          args: ev.args,
        };
        return { ...step, iterations };
      });
    case "fixLogUpdated":
      return updateStep(s, ev.stepId, (step) => {
        if (step.status.stage !== "iterate") return step;
        return { ...step, status: { ...step.status, fix_log: ev.fixLog.map(wireFix) } };
      });
    case "currentPrincipleChanged":
      return updateStep(s, ev.stepId, (step) => {
        if (step.status.stage !== "iterate") return step;
        return {
          ...step,
          status: { ...step.status, current_principle: ev.principle ?? undefined },
        };
      });
    case "executeStarted":
      return updateStep(s, ev.stepId, (step) => ({
        ...step,
        status: { stage: "execute", plan: wirePlanToView(ev.plan) },
      }));
    case "executeCompleted":
      return updateStep(s, ev.stepId, (step) => {
        const plan = currentPlan(step.status);
        if (!plan) return step;
        return { ...step, status: { stage: "summarize", plan, output: ev.output } };
      });
    case "summarizeCompleted":
      return updateStep(s, ev.stepId, (step) => {
        if (step.status.stage !== "summarize") return step;
        return {
          ...step,
          status: {
            stage: "done",
            plan: step.status.plan,
            summary: ev.summary,
            output: step.status.output,
          },
        };
      });
    case "stepCompleted":
      return s;
    case "persistentMustHaveFailure":
      return updateStep(s, ev.stepId, (step) => ({
        ...step,
        persistent_failure: { principle: ev.principle, attempts: ev.attempts },
      }));
    case "stateChanged":
      return { ...s, state: ev.state };
    case "turnFinished":
      return { ...s, inFlight: false };
    default:
      return assertNever(ev);
  }
}

// ─── helpers ──────────────────────────────────────────────────────────

function appendStep(s: Snapshot, stepId: bigint, status: StepStatus): Snapshot {
  const id = stepIdToString(stepId);
  if (s.turns.some((t) => t.kind === "step" && t.id === id)) return s;
  const step: Step = { id, status, iterations: [] };
  return { ...s, turns: [...s.turns, { kind: "step", id, step }] };
}

function updateStep(s: Snapshot, stepId: bigint, fn: (step: Step) => Step): Snapshot {
  const id = stepIdToString(stepId);
  let touched = false;
  const turns = s.turns.map((t) => {
    if (t.kind !== "step" || t.id !== id) return t;
    touched = true;
    return { ...t, step: fn(t.step) };
  });
  if (!touched) return s;
  return { ...s, turns };
}

function currentPlan(status: StepStatus): Plan | undefined {
  if (status.stage === "plan") return undefined;
  return status.plan;
}

function replaceOrAppendIteration(
  iters: IterationRecord[],
  it: IterationRecord,
): IterationRecord[] {
  const i = iters.findIndex((x) => x.index === it.index);
  if (i < 0) return [...iters, it];
  const out = iters.slice();
  out[i] = it;
  return out;
}

function wirePlanToView(p: WirePlan): Plan {
  return {
    tool: p.tool,
    goal: p.goal,
    why_this_tool: p.whyThisTool,
    considerations: p.considerations,
    args: p.args,
  };
}

function wireVerdictToView(v: WireVerdict): Verdict {
  switch (v.kind) {
    case "pass":
      return { kind: "pass" };
    case "fix":
      return { kind: "fix", feedback: v.feedback };
    case "rethink":
      return { kind: "rethink", feedback: v.feedback };
    default:
      return assertNever(v);
  }
}

function wireFix(f: FixEntry): { principle: string; feedback: string } {
  return { principle: f.principle, feedback: f.feedback };
}

function wireTurnToView(t: WireTurn): Turn {
  if (t.kind === "user") return { kind: "user", id: t.id, text: t.text };
  return { kind: "step", id: stepIdToString(t.step.id), step: wireStepToView(t.step) };
}

function wireStepToView(s: WireStep): Step {
  return {
    id: stepIdToString(s.id),
    status: wireStepStatusToView(s.status),
    iterations: [], // iteration history isn't replayed on subscribe yet
    persistent_failure: s.persistentFailure
      ? wirePersistentFailure(s.persistentFailure)
      : undefined,
  };
}

function wireStepStatusToView(s: WireStepStatus): StepStatus {
  switch (s.kind) {
    case "plan":
      return { stage: "plan" };
    case "iterate":
      return {
        stage: "iterate",
        plan: wirePlanToView(s.plan),
        fix_log: s.fixLog.map(wireFix),
        current_principle: s.currentPrinciple ?? undefined,
      };
    case "execute":
      return { stage: "execute", plan: wirePlanToView(s.plan) };
    case "summarize":
      return { stage: "summarize", plan: wirePlanToView(s.plan), output: s.output };
    case "done":
      return {
        stage: "done",
        plan: wirePlanToView(s.plan),
        summary: s.summary,
        output: s.output,
      };
    default:
      return assertNever(s);
  }
}

function wirePersistentFailure(f: PersistentFailure): {
  principle: string;
  attempts: number;
} {
  return { principle: f.principle, attempts: f.attempts };
}

function stepIdToString(id: bigint): string {
  return `s-${id.toString()}`;
}

let counter = 0;
function optimisticId(): string {
  counter += 1;
  return `u-pending-${counter}`;
}

function errorMessage(e: ChatError): string {
  switch (e.kind) {
    case "internal":
      return e.message;
    case "turnInFlight":
      return "a turn is already in flight";
    case "noTurnInFlight":
      return "no turn in flight";
    case "personaNotFound":
      return `persona not found: ${e.name}`;
    default:
      return assertNever(e);
  }
}

function assertNever(x: never): never {
  throw new Error(`unreachable: ${JSON.stringify(x)}`);
}
