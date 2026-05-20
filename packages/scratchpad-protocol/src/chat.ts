// Scratchpad workflow wire protocol. Mirrors `workflows/scratchpad/src/
// wire.rs`. Variant indices follow declared order — keep this file in
// sync if engine-side enums grow new arms.

import * as pc from "./postcard";

export type TurnId = bigint;
export type StepId = bigint;

export interface SessionState {
  persona: string | null;
}

export interface PersonaInfo {
  name: string;
  displayName: string;
  model: string;
}

export interface Plan {
  tool: string;
  goal: string;
  whyThisTool: string;
  considerations: string[];
  /** Parsed once at the wire boundary — `unknown` so consumers
   *  discriminate before reading fields. `null` if the engine's raw
   *  JSON didn't parse (would be an upstream bug). */
  args: unknown;
}

export type Verdict =
  | { kind: "pass" }
  | { kind: "fix"; feedback: string }
  | { kind: "rethink"; feedback: string };

export interface FixEntry {
  principle: string;
  feedback: string;
}

export interface PrincipleVerdict {
  principle: string;
  verdict: Verdict;
}

export interface Iteration {
  index: number;
  args: unknown;
  principleVerdicts: PrincipleVerdict[];
}

export type StepStatus =
  | { kind: "plan" }
  | {
      kind: "iterate";
      plan: Plan;
      fixLog: FixEntry[];
      currentPrinciple: string | null;
    }
  | { kind: "execute"; plan: Plan }
  | { kind: "summarize"; plan: Plan; output: string }
  | { kind: "done"; plan: Plan; summary: string; output: string };

export interface PersistentFailure {
  principle: string;
  attempts: number;
}

export interface Step {
  id: StepId;
  status: StepStatus;
  iterations: Iteration[];
  persistentFailure: PersistentFailure | null;
}

export type Turn =
  | { kind: "user"; id: string; text: string }
  | { kind: "step"; step: Step };

export type FinishReason =
  | { kind: "completed" }
  | { kind: "cancelled" }
  | { kind: "failed"; message: string };

export type ChatRequest =
  | { kind: "subscribe" }
  | { kind: "sendMessage"; text: string }
  | { kind: "cancel" }
  | { kind: "setPersona"; name: string | null }
  | { kind: "listPersonas" }
  | { kind: "getState" };

export type ChatOk =
  | { kind: "subscribed"; state: SessionState; turns: Turn[] }
  | { kind: "messageQueued"; turnId: TurnId }
  | { kind: "cancelled" }
  | { kind: "state"; state: SessionState }
  | { kind: "stateUpdated"; state: SessionState }
  | { kind: "personas"; personas: PersonaInfo[] };

export type ChatError =
  | { kind: "internal"; message: string }
  | { kind: "turnInFlight" }
  | { kind: "noTurnInFlight" }
  | { kind: "personaNotFound"; name: string };

export type ChatResponse =
  | { ok: true; value: ChatOk }
  | { ok: false; error: ChatError };

export type ChatEvent =
  | { kind: "userMessageAppended"; id: string; text: string }
  | { kind: "assistantMessage"; id: string; text: string }
  | { kind: "stepStarted"; stepId: StepId }
  | { kind: "planProposed"; stepId: StepId; plan: Plan }
  | { kind: "planRethink"; stepId: StepId; feedback: string }
  | { kind: "iterationStarted"; stepId: StepId; index: number; args: unknown }
  | {
      kind: "principleEvaluated";
      stepId: StepId;
      iteration: number;
      principle: string;
      verdict: Verdict;
    }
  | { kind: "scratchpadEdited"; stepId: StepId; args: unknown }
  | { kind: "fixLogUpdated"; stepId: StepId; fixLog: FixEntry[] }
  | {
      kind: "currentPrincipleChanged";
      stepId: StepId;
      principle: string | null;
    }
  | { kind: "executeStarted"; stepId: StepId; plan: Plan }
  | { kind: "executeCompleted"; stepId: StepId; output: string }
  | { kind: "summarizeCompleted"; stepId: StepId; summary: string }
  | { kind: "stepCompleted"; stepId: StepId }
  | {
      kind: "persistentMustHaveFailure";
      stepId: StepId;
      principle: string;
      attempts: number;
    }
  | { kind: "stateChanged"; state: SessionState }
  | { kind: "turnFinished"; turnId: TurnId; reason: FinishReason };

// ─── helpers ──────────────────────────────────────────────────────────

function tryParseJson(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function writeJsonString(w: pc.Writer, v: unknown): void {
  pc.writeString(w, JSON.stringify(v ?? null));
}

// ─── readers (decode broadcasts + responses) ──────────────────────────

function readSessionState(r: pc.Reader): SessionState {
  return { persona: pc.readOption(r, pc.readString) };
}

function readPersonaInfo(r: pc.Reader): PersonaInfo {
  return {
    name: pc.readString(r),
    displayName: pc.readString(r),
    model: pc.readString(r),
  };
}

function readPlan(r: pc.Reader): Plan {
  return {
    tool: pc.readString(r),
    goal: pc.readString(r),
    whyThisTool: pc.readString(r),
    considerations: pc.readVec(r, pc.readString),
    args: tryParseJson(pc.readString(r)),
  };
}

function readVerdict(r: pc.Reader): Verdict {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "pass" };
    case 1:
      return { kind: "fix", feedback: pc.readString(r) };
    case 2:
      return { kind: "rethink", feedback: pc.readString(r) };
    default:
      throw new Error(`postcard: invalid Verdict ${v}`);
  }
}

function readFixEntry(r: pc.Reader): FixEntry {
  return { principle: pc.readString(r), feedback: pc.readString(r) };
}

function readPrincipleVerdict(r: pc.Reader): PrincipleVerdict {
  return { principle: pc.readString(r), verdict: readVerdict(r) };
}

function readIteration(r: pc.Reader): Iteration {
  return {
    index: pc.readU32(r),
    args: tryParseJson(pc.readString(r)),
    principleVerdicts: pc.readVec(r, readPrincipleVerdict),
  };
}

function readStepStatus(r: pc.Reader): StepStatus {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "plan" };
    case 1:
      return {
        kind: "iterate",
        plan: readPlan(r),
        fixLog: pc.readVec(r, readFixEntry),
        currentPrinciple: pc.readOption(r, pc.readString),
      };
    case 2:
      return { kind: "execute", plan: readPlan(r) };
    case 3:
      return {
        kind: "summarize",
        plan: readPlan(r),
        output: pc.readString(r),
      };
    case 4:
      return {
        kind: "done",
        plan: readPlan(r),
        summary: pc.readString(r),
        output: pc.readString(r),
      };
    default:
      throw new Error(`postcard: invalid StepStatus ${v}`);
  }
}

function readPersistentFailure(r: pc.Reader): PersistentFailure {
  return { principle: pc.readString(r), attempts: pc.readU32(r) };
}

function readStep(r: pc.Reader): Step {
  return {
    id: pc.readU64(r),
    status: readStepStatus(r),
    iterations: pc.readVec(r, readIteration),
    persistentFailure: pc.readOption(r, readPersistentFailure),
  };
}

function readTurn(r: pc.Reader): Turn {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "user", id: pc.readString(r), text: pc.readString(r) };
    case 1:
      return { kind: "step", step: readStep(r) };
    default:
      throw new Error(`postcard: invalid Turn ${v}`);
  }
}

function readFinishReason(r: pc.Reader): FinishReason {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "completed" };
    case 1:
      return { kind: "cancelled" };
    case 2:
      return { kind: "failed", message: pc.readString(r) };
    default:
      throw new Error(`postcard: invalid FinishReason ${v}`);
  }
}

function readChatOk(r: pc.Reader): ChatOk {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return {
        kind: "subscribed",
        state: readSessionState(r),
        turns: pc.readVec(r, readTurn),
      };
    case 1:
      return { kind: "messageQueued", turnId: pc.readU64(r) };
    case 2:
      return { kind: "cancelled" };
    case 3:
      return { kind: "state", state: readSessionState(r) };
    case 4:
      return { kind: "stateUpdated", state: readSessionState(r) };
    case 5:
      return { kind: "personas", personas: pc.readVec(r, readPersonaInfo) };
    default:
      throw new Error(`postcard: invalid ChatOk ${v}`);
  }
}

function readChatError(r: pc.Reader): ChatError {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "internal", message: pc.readString(r) };
    case 1:
      return { kind: "turnInFlight" };
    case 2:
      return { kind: "noTurnInFlight" };
    case 3:
      return { kind: "personaNotFound", name: pc.readString(r) };
    default:
      throw new Error(`postcard: invalid ChatError ${v}`);
  }
}

export function decodeChatResponse(bytes: Uint8Array): ChatResponse {
  const r = new pc.Reader(bytes);
  const tag = pc.readVariant(r);
  if (tag === 0) return { ok: true, value: readChatOk(r) };
  if (tag === 1) return { ok: false, error: readChatError(r) };
  throw new Error(`postcard: invalid Result tag ${tag}`);
}

export function decodeChatEvent(bytes: Uint8Array): ChatEvent {
  const r = new pc.Reader(bytes);
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return {
        kind: "userMessageAppended",
        id: pc.readString(r),
        text: pc.readString(r),
      };
    case 1:
      return {
        kind: "assistantMessage",
        id: pc.readString(r),
        text: pc.readString(r),
      };
    case 2:
      return { kind: "stepStarted", stepId: pc.readU64(r) };
    case 3:
      return {
        kind: "planProposed",
        stepId: pc.readU64(r),
        plan: readPlan(r),
      };
    case 4:
      return {
        kind: "planRethink",
        stepId: pc.readU64(r),
        feedback: pc.readString(r),
      };
    case 5:
      return {
        kind: "iterationStarted",
        stepId: pc.readU64(r),
        index: pc.readU32(r),
        args: tryParseJson(pc.readString(r)),
      };
    case 6:
      return {
        kind: "principleEvaluated",
        stepId: pc.readU64(r),
        iteration: pc.readU32(r),
        principle: pc.readString(r),
        verdict: readVerdict(r),
      };
    case 7:
      return {
        kind: "scratchpadEdited",
        stepId: pc.readU64(r),
        args: tryParseJson(pc.readString(r)),
      };
    case 8:
      return {
        kind: "fixLogUpdated",
        stepId: pc.readU64(r),
        fixLog: pc.readVec(r, readFixEntry),
      };
    case 9:
      return {
        kind: "currentPrincipleChanged",
        stepId: pc.readU64(r),
        principle: pc.readOption(r, pc.readString),
      };
    case 10:
      return {
        kind: "executeStarted",
        stepId: pc.readU64(r),
        plan: readPlan(r),
      };
    case 11:
      return {
        kind: "executeCompleted",
        stepId: pc.readU64(r),
        output: pc.readString(r),
      };
    case 12:
      return {
        kind: "summarizeCompleted",
        stepId: pc.readU64(r),
        summary: pc.readString(r),
      };
    case 13:
      return { kind: "stepCompleted", stepId: pc.readU64(r) };
    case 14:
      return {
        kind: "persistentMustHaveFailure",
        stepId: pc.readU64(r),
        principle: pc.readString(r),
        attempts: pc.readU32(r),
      };
    case 15:
      return { kind: "stateChanged", state: readSessionState(r) };
    case 16:
      return {
        kind: "turnFinished",
        turnId: pc.readU64(r),
        reason: readFinishReason(r),
      };
    default:
      throw new Error(`postcard: invalid ChatEvent ${v}`);
  }
}

// ─── writers (encode requests) ────────────────────────────────────────

export function encodeChatRequest(req: ChatRequest): Uint8Array {
  const w = new pc.Writer();
  switch (req.kind) {
    case "subscribe":
      pc.writeVariant(w, 0);
      break;
    case "sendMessage":
      pc.writeVariant(w, 1);
      pc.writeString(w, req.text);
      break;
    case "cancel":
      pc.writeVariant(w, 2);
      break;
    case "setPersona":
      pc.writeVariant(w, 3);
      pc.writeOption(w, req.name, pc.writeString);
      break;
    case "listPersonas":
      pc.writeVariant(w, 4);
      break;
    case "getState":
      pc.writeVariant(w, 5);
      break;
  }
  return w.finish();
}

// Exported so a Node-side codec parity test can stress-test the
// roundtrip via the engine binary if one is added later.
export { writeJsonString };
