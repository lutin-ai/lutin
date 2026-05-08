// Chat protocol postcard bindings. Mirrors the Rust types in
// `workflows/chat/src/lib.rs`. Variant indices follow declared order;
// keep this file in sync if engine-side enums grow new arms.

import * as pc from "./postcard";

export type TurnId = bigint;

export type FinishReason =
  | { kind: "completed" }
  | { kind: "cancelled" }
  | { kind: "failed"; message: string };

export interface SessionState {
  persona: string | null;
  modelOverride: string | null;
}

export type HistoricalRole = "user" | "assistant" | "thinking";

export interface HistoricalMessage {
  role: HistoricalRole;
  text: string;
}

export type ChatRequest =
  | { kind: "subscribe" }
  | { kind: "sendMessage"; text: string }
  | { kind: "cancel" }
  | { kind: "setPersona"; name: string | null }
  | { kind: "getState" }
  | { kind: "listPersonas" }
  | { kind: "rerun" };

export interface PersonaInfo {
  name: string;
  displayName: string;
  /** Empty string when the persona doesn't pin a model. */
  model: string;
}

export type ChatOk =
  | { kind: "subscribed"; state: SessionState; history: HistoricalMessage[] }
  | { kind: "messageQueued"; turnId: TurnId }
  | { kind: "cancelled" }
  | { kind: "stateUpdated"; state: SessionState }
  | { kind: "state"; state: SessionState }
  | { kind: "personas"; personas: PersonaInfo[] };

export type ChatError =
  | { kind: "noTurnInFlight" }
  | { kind: "personaNotFound"; name: string }
  | { kind: "providerNotFound"; name: string }
  | { kind: "providerMisconfigured"; name: string; reason: string }
  | { kind: "providerUnsupported"; providerKind: string }
  | { kind: "internal"; message: string };

export type ChatResponse =
  | { ok: true; value: ChatOk }
  | { ok: false; error: ChatError };

export type ChatEvent =
  | { kind: "delta"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "toolCallStarted"; id: string; name: string }
  | { kind: "toolCallCompleted"; id: string; ok: boolean; summary: string }
  | { kind: "messageFinished"; turnId: TurnId; reason: FinishReason }
  | { kind: "stateChanged"; state: SessionState };

// ─── SessionState / HistoricalMessage ────────────────────────────────

function readSessionState(r: pc.Reader): SessionState {
  return {
    persona: pc.readOption(r, pc.readString),
    modelOverride: pc.readOption(r, pc.readString),
  };
}

function readHistoricalRole(r: pc.Reader): HistoricalRole {
  const v = pc.readVariant(r);
  if (v === 0) return "user";
  if (v === 1) return "assistant";
  if (v === 2) return "thinking";
  throw new Error(`postcard: invalid HistoricalRole ${v}`);
}

function readHistoricalMessage(r: pc.Reader): HistoricalMessage {
  return { role: readHistoricalRole(r), text: pc.readString(r) };
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

// ─── ChatRequest (encode only — the engine reads these) ──────────────

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
    case "getState":
      pc.writeVariant(w, 4);
      break;
    case "listPersonas":
      pc.writeVariant(w, 5);
      break;
    case "rerun":
      pc.writeVariant(w, 6);
      break;
  }
  return w.finish();
}

// ─── ChatResponse / ChatOk / ChatError (decode only) ─────────────────

export function decodeChatResponse(bytes: Uint8Array): ChatResponse {
  const r = new pc.Reader(bytes);
  const tag = pc.readVariant(r);
  if (tag === 0) return { ok: true, value: readChatOk(r) };
  if (tag === 1) return { ok: false, error: readChatError(r) };
  throw new Error(`postcard: invalid Result tag ${tag}`);
}

function readChatOk(r: pc.Reader): ChatOk {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return {
        kind: "subscribed",
        state: readSessionState(r),
        history: pc.readVec(r, readHistoricalMessage),
      };
    case 1:
      return { kind: "messageQueued", turnId: pc.readU64(r) };
    case 2:
      return { kind: "cancelled" };
    case 3:
      return { kind: "stateUpdated", state: readSessionState(r) };
    case 4:
      return { kind: "state", state: readSessionState(r) };
    case 5:
      return {
        kind: "personas",
        personas: pc.readVec(r, readPersonaInfo),
      };
    default:
      throw new Error(`postcard: invalid ChatOk ${v}`);
  }
}

function readPersonaInfo(r: pc.Reader): PersonaInfo {
  return {
    name: pc.readString(r),
    displayName: pc.readString(r),
    model: pc.readString(r),
  };
}

function readChatError(r: pc.Reader): ChatError {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "noTurnInFlight" };
    case 1:
      return { kind: "personaNotFound", name: pc.readString(r) };
    case 2:
      return { kind: "providerNotFound", name: pc.readString(r) };
    case 3:
      return {
        kind: "providerMisconfigured",
        name: pc.readString(r),
        reason: pc.readString(r),
      };
    case 4:
      return { kind: "providerUnsupported", providerKind: pc.readString(r) };
    case 5:
      return { kind: "internal", message: pc.readString(r) };
    default:
      throw new Error(`postcard: invalid ChatError ${v}`);
  }
}

export function chatErrorMessage(err: ChatError): string {
  switch (err.kind) {
    case "noTurnInFlight":
      return "no turn in progress";
    case "personaNotFound":
      return `persona not found: ${err.name}`;
    case "providerNotFound":
      return `provider not configured: ${err.name}`;
    case "providerMisconfigured":
      return `provider '${err.name}' misconfigured: ${err.reason}`;
    case "providerUnsupported":
      return `provider kind unsupported: ${err.providerKind}`;
    case "internal":
      return `internal: ${err.message}`;
  }
}

// ─── ChatEvent (decode only) ─────────────────────────────────────────

export function decodeChatEvent(bytes: Uint8Array): ChatEvent {
  const r = new pc.Reader(bytes);
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "delta", text: pc.readString(r) };
    case 1:
      return { kind: "reasoning", text: pc.readString(r) };
    case 2:
      return {
        kind: "toolCallStarted",
        id: pc.readString(r),
        name: pc.readString(r),
      };
    case 3:
      return {
        kind: "toolCallCompleted",
        id: pc.readString(r),
        ok: pc.readBool(r),
        summary: pc.readString(r),
      };
    case 4:
      return {
        kind: "messageFinished",
        turnId: pc.readU64(r),
        reason: readFinishReason(r),
      };
    case 5:
      return { kind: "stateChanged", state: readSessionState(r) };
    default:
      throw new Error(`postcard: invalid ChatEvent ${v}`);
  }
}
