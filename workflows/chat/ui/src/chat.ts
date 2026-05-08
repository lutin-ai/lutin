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

/** Result of a tool call. Two-variant union — `ok` text and `failed`
 *  text are different concepts, so they get different shapes. */
export type ToolOutcome =
  | { kind: "ok"; text: string }
  | { kind: "failed"; text: string };

export type HistoricalMessage =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "thinking"; text: string }
  | {
      kind: "tool";
      callId: string;
      name: string;
      /** Parsed once at the wire boundary — `unknown` so consumers know
       *  to discriminate before reading fields. `null` if the engine's
       *  raw JSON didn't parse (rare; would be an upstream bug). */
      arguments: unknown;
      /** `null` for the mid-turn snapshot case where a tool was emitted
       *  but no result has come back yet. */
      outcome: ToolOutcome | null;
    }
  | { kind: "subAgentReply"; agentId: string; text: string }
  | { kind: "subAgentFailure"; agentId: string; reason: string };

/** Live sub-agent registry row. Mirrors `chat::SubAgentInfo`. */
export interface SubAgentInfo {
  id: string;
  /** `null` for top-level children of the main session; the parent
   *  `agent#N` id when one sub-agent spawned this one. */
  parentId: string | null;
  persona: string;
  status: SubAgentStatus;
  lastProgress: string | null;
}

export type SubAgentStatus =
  | { kind: "running" }
  | { kind: "completed" }
  | { kind: "failed"; reason: string }
  | { kind: "stopped" };

export type ChatRequest =
  | { kind: "subscribe" }
  | { kind: "sendMessage"; text: string }
  | { kind: "cancel" }
  | { kind: "setPersona"; name: string | null }
  | { kind: "getState" }
  | { kind: "listPersonas" }
  | { kind: "rerun" }
  | { kind: "editMessage"; index: number; text: string }
  | { kind: "deleteMessage"; index: number }
  | { kind: "deleteFromHere"; index: number }
  | { kind: "getMetrics" }
  | { kind: "listSubAgents" }
  | { kind: "getSubAgentTranscript"; id: string };

/** Per-projected-entry metrics. One variant per `HistoricalMessage`
 *  kind, in declared variant order. Each variant carries only the
 *  fields its kind can validly produce, so e.g. a `User` row can't
 *  accidentally encode token counts. Timestamps decode as RFC3339
 *  strings (or `null` for transcripts loaded before metrics existed);
 *  millisecond durations decode as `number` after a safe-integer
 *  bounds check at the wire boundary, so downstream code never
 *  touches `bigint`. */
export type MessageMeta =
  | { kind: "user"; timestamp: string | null }
  | {
      kind: "assistant";
      timestamp: string | null;
      ttftMs: number | null;
      durationMs: number | null;
      promptTokens: number | null;
      completionTokens: number | null;
    }
  | {
      kind: "thinking";
      timestamp: string | null;
      ttftMs: number | null;
      durationMs: number | null;
    }
  | { kind: "tool"; timestamp: string | null; durationMs: number | null }
  | { kind: "subAgentReply"; timestamp: string | null }
  | { kind: "subAgentFailure"; timestamp: string | null };

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
  | { kind: "personas"; personas: PersonaInfo[] }
  | { kind: "historyAcknowledged" }
  | { kind: "metrics"; metrics: MessageMeta[] }
  | { kind: "subAgents"; subAgents: SubAgentInfo[] }
  | { kind: "subAgentTranscript"; id: string; history: HistoricalMessage[] };

export type ChatError =
  | { kind: "noTurnInFlight" }
  | { kind: "personaNotFound"; name: string }
  | { kind: "providerNotFound"; name: string }
  | { kind: "providerMisconfigured"; name: string; reason: string }
  | { kind: "providerUnsupported"; providerKind: string }
  | { kind: "internal"; message: string }
  | { kind: "turnInFlight" }
  | { kind: "historyIndexOutOfRange"; index: number }
  | { kind: "persistFailed"; op: string };

export type ChatResponse =
  | { ok: true; value: ChatOk }
  | { ok: false; error: ChatError };

export type ChatEvent =
  | { kind: "delta"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "toolCallStarted"; id: string; name: string; arguments: unknown }
  | { kind: "toolCallCompleted"; id: string; outcome: ToolOutcome }
  | { kind: "messageFinished"; turnId: TurnId; reason: FinishReason }
  | { kind: "stateChanged"; state: SessionState }
  | { kind: "historyReplaced"; history: HistoricalMessage[] }
  | { kind: "metricsReplaced"; metrics: MessageMeta[] }
  | { kind: "subAgentsChanged"; subAgents: SubAgentInfo[] }
  | { kind: "subAgentTranscriptUpdated"; id: string; history: HistoricalMessage[] };

// ─── SessionState / HistoricalMessage ────────────────────────────────

function readSessionState(r: pc.Reader): SessionState {
  return {
    persona: pc.readOption(r, pc.readString),
    modelOverride: pc.readOption(r, pc.readString),
  };
}

function readToolOutcome(r: pc.Reader): ToolOutcome {
  const v = pc.readVariant(r);
  if (v === 0) return { kind: "ok", text: pc.readString(r) };
  if (v === 1) return { kind: "failed", text: pc.readString(r) };
  throw new Error(`postcard: invalid ToolOutcome ${v}`);
}

function tryParseJson(raw: string): unknown {
  // Engine-side `serde_json::to_string(&Value)` is infallible, so a
  // parse failure here means the wire was corrupted — return null and
  // let the UI render a placeholder rather than crash the decoder.
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function readHistoricalMessage(r: pc.Reader): HistoricalMessage {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "user", text: pc.readString(r) };
    case 1:
      return { kind: "assistant", text: pc.readString(r) };
    case 2:
      return { kind: "thinking", text: pc.readString(r) };
    case 3:
      return {
        kind: "tool",
        callId: pc.readString(r),
        name: pc.readString(r),
        arguments: tryParseJson(pc.readString(r)),
        outcome: pc.readOption(r, readToolOutcome),
      };
    case 4:
      return {
        kind: "subAgentReply",
        agentId: pc.readString(r),
        text: pc.readString(r),
      };
    case 5:
      return {
        kind: "subAgentFailure",
        agentId: pc.readString(r),
        reason: pc.readString(r),
      };
    default:
      throw new Error(`postcard: invalid HistoricalMessage ${v}`);
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
    case "editMessage":
      pc.writeVariant(w, 7);
      pc.writeU32(w, req.index);
      pc.writeString(w, req.text);
      break;
    case "deleteMessage":
      pc.writeVariant(w, 8);
      pc.writeU32(w, req.index);
      break;
    case "deleteFromHere":
      pc.writeVariant(w, 9);
      pc.writeU32(w, req.index);
      break;
    case "getMetrics":
      pc.writeVariant(w, 10);
      break;
    case "listSubAgents":
      pc.writeVariant(w, 11);
      break;
    case "getSubAgentTranscript":
      pc.writeVariant(w, 12);
      pc.writeString(w, req.id);
      break;
  }
  return w.finish();
}

function readSubAgentStatus(r: pc.Reader): SubAgentStatus {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "running" };
    case 1:
      return { kind: "completed" };
    case 2:
      return { kind: "failed", reason: pc.readString(r) };
    case 3:
      return { kind: "stopped" };
    default:
      throw new Error(`postcard: invalid SubAgentStatus ${v}`);
  }
}

function readSubAgentInfo(r: pc.Reader): SubAgentInfo {
  return {
    id: pc.readString(r),
    parentId: pc.readOption(r, pc.readString),
    persona: pc.readString(r),
    status: readSubAgentStatus(r),
    lastProgress: pc.readOption(r, pc.readString),
  };
}

/** Convert a postcard `Option<u64>` to `number | null`, rejecting values
 *  past `Number.MAX_SAFE_INTEGER` so the wire layer can't sneak a lossy
 *  truncation into UI code. ms durations stay safe for millennia. */
function readU64Ms(r: pc.Reader): number | null {
  const raw = pc.readOption(r, pc.readU64);
  if (raw === null) return null;
  if (raw > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`metrics: duration ${raw} exceeds safe integer range`);
  }
  return Number(raw);
}

function readMessageMeta(r: pc.Reader): MessageMeta {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "user", timestamp: pc.readOption(r, pc.readString) };
    case 1:
      return {
        kind: "assistant",
        timestamp: pc.readOption(r, pc.readString),
        ttftMs: readU64Ms(r),
        durationMs: readU64Ms(r),
        promptTokens: pc.readOption(r, pc.readU32),
        completionTokens: pc.readOption(r, pc.readU32),
      };
    case 2:
      return {
        kind: "thinking",
        timestamp: pc.readOption(r, pc.readString),
        ttftMs: readU64Ms(r),
        durationMs: readU64Ms(r),
      };
    case 3:
      return {
        kind: "tool",
        timestamp: pc.readOption(r, pc.readString),
        durationMs: readU64Ms(r),
      };
    case 4:
      return { kind: "subAgentReply", timestamp: pc.readOption(r, pc.readString) };
    case 5:
      return { kind: "subAgentFailure", timestamp: pc.readOption(r, pc.readString) };
    default:
      throw new Error(`postcard: invalid MessageMeta ${v}`);
  }
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
    case 6:
      return { kind: "historyAcknowledged" };
    case 7:
      return { kind: "metrics", metrics: pc.readVec(r, readMessageMeta) };
    case 8:
      return { kind: "subAgents", subAgents: pc.readVec(r, readSubAgentInfo) };
    case 9:
      return {
        kind: "subAgentTranscript",
        id: pc.readString(r),
        history: pc.readVec(r, readHistoricalMessage),
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
    case 6:
      return { kind: "turnInFlight" };
    case 7:
      return { kind: "historyIndexOutOfRange", index: pc.readU32(r) };
    case 8:
      return { kind: "persistFailed", op: pc.readString(r) };
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
    case "turnInFlight":
      return "a turn is in flight; cancel it before mutating history";
    case "historyIndexOutOfRange":
      return `history index out of range: ${err.index}`;
    case "persistFailed":
      return `failed to ${err.op}; the change is in memory but not on disk`;
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
        arguments: tryParseJson(pc.readString(r)),
      };
    case 3:
      return {
        kind: "toolCallCompleted",
        id: pc.readString(r),
        outcome: readToolOutcome(r),
      };
    case 4:
      return {
        kind: "messageFinished",
        turnId: pc.readU64(r),
        reason: readFinishReason(r),
      };
    case 5:
      return { kind: "stateChanged", state: readSessionState(r) };
    case 6:
      return {
        kind: "historyReplaced",
        history: pc.readVec(r, readHistoricalMessage),
      };
    case 7:
      return { kind: "metricsReplaced", metrics: pc.readVec(r, readMessageMeta) };
    case 8:
      return {
        kind: "subAgentsChanged",
        subAgents: pc.readVec(r, readSubAgentInfo),
      };
    case 9:
      return {
        kind: "subAgentTranscriptUpdated",
        id: pc.readString(r),
        history: pc.readVec(r, readHistoricalMessage),
      };
    default:
      throw new Error(`postcard: invalid ChatEvent ${v}`);
  }
}
