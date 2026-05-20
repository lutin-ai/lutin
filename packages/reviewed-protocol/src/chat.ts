// Reviewed workflow wire protocol. Mirrors `workflows/reviewed/src/
// wire.rs`. Variant indices follow declared order — keep this file in
// sync if engine-side enums grow new arms.

import * as pc from "./postcard";

export type TurnId = bigint;

export interface SessionState {
  persona: string | null;
}

export interface PersonaInfo {
  name: string;
  displayName: string;
  model: string;
}

export type ReviewVerdict =
  | { kind: "pass" }
  | { kind: "fix"; feedback: string }
  | { kind: "rethink"; feedback: string };

export type Turn =
  | { kind: "user"; id: string; text: string }
  | { kind: "assistant"; id: string; text: string }
  | {
      kind: "toolCall";
      id: string;
      tool: string;
      args: unknown;
      output: string;
    };

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
  | {
      kind: "toolCallDrafted";
      stepId: bigint;
      attempt: number;
      tool: string;
      args: unknown;
    }
  | {
      kind: "principleEvaluated";
      stepId: bigint;
      attempt: number;
      principle: string;
      verdict: ReviewVerdict;
    }
  | {
      kind: "toolCallExecuted";
      stepId: bigint;
      tool: string;
      args: unknown;
      output: string;
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

// ─── readers ──────────────────────────────────────────────────────────

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

function readReviewVerdict(r: pc.Reader): ReviewVerdict {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "pass" };
    case 1:
      return { kind: "fix", feedback: pc.readString(r) };
    case 2:
      return { kind: "rethink", feedback: pc.readString(r) };
    default:
      throw new Error(`postcard: invalid ReviewVerdict ${v}`);
  }
}

function readTurn(r: pc.Reader): Turn {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "user", id: pc.readString(r), text: pc.readString(r) };
    case 1:
      return { kind: "assistant", id: pc.readString(r), text: pc.readString(r) };
    case 2:
      return {
        kind: "toolCall",
        id: pc.readString(r),
        tool: pc.readString(r),
        args: tryParseJson(pc.readString(r)),
        output: pc.readString(r),
      };
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
      return {
        kind: "toolCallDrafted",
        stepId: pc.readU64(r),
        attempt: pc.readU32(r),
        tool: pc.readString(r),
        args: tryParseJson(pc.readString(r)),
      };
    case 3:
      return {
        kind: "principleEvaluated",
        stepId: pc.readU64(r),
        attempt: pc.readU32(r),
        principle: pc.readString(r),
        verdict: readReviewVerdict(r),
      };
    case 4:
      return {
        kind: "toolCallExecuted",
        stepId: pc.readU64(r),
        tool: pc.readString(r),
        args: tryParseJson(pc.readString(r)),
        output: pc.readString(r),
      };
    case 5:
      return { kind: "stateChanged", state: readSessionState(r) };
    case 6:
      return {
        kind: "turnFinished",
        turnId: pc.readU64(r),
        reason: readFinishReason(r),
      };
    default:
      throw new Error(`postcard: invalid ChatEvent ${v}`);
  }
}

// ─── writers ──────────────────────────────────────────────────────────

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
