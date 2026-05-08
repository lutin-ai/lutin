// Normalized data shapes the chat widgets render. Workflows adapt their
// protocol-specific snapshots into these and feed them to <ChatView />.

export type Role = "user" | "assistant" | "system";

/** Per-message metrics rendered as a small footer. The adapter parses
 *  the wire RFC3339 string into a `Date` once at the boundary, so the
 *  widget never has to re-parse. Missing fields suppress their chip;
 *  if every field is missing the adapter returns `undefined` and no
 *  footer is rendered at all. */
export interface MessageMeta {
  /** Wall-clock when the entry was recorded; `null` for legacy
   *  transcripts loaded before metrics existed. */
  time: Date | null;
  /** Time-to-first-token (assistant text/thinking only). */
  ttftMs?: number | null;
  /** Full turn duration (assistant) or per-call duration (tool). */
  durationMs?: number | null;
  /** Input tokens (assistant text only). */
  promptTokens?: number | null;
  /** Output tokens (assistant text only). */
  completionTokens?: number | null;
}

export interface UserMessage {
  kind: "user";
  id?: string;
  text: string;
  meta?: MessageMeta;
}

export interface AssistantMessage {
  kind: "assistant";
  id?: string;
  text: string;
  /** True when this is the in-flight message currently being streamed. */
  streaming?: boolean;
  meta?: MessageMeta;
}

export interface SystemMessage {
  kind: "system";
  id?: string;
  text: string;
  meta?: MessageMeta;
}

export interface ThinkingMessage {
  kind: "thinking";
  id?: string;
  text: string;
  /** True while reasoning is actively streaming — the widget defaults
   *  this open so users see tokens as they arrive. Completed/historical
   *  thinking defaults closed to keep the transcript scannable. */
  streaming?: boolean;
  meta?: MessageMeta;
}

export interface ToolCallMessage {
  kind: "toolCall";
  id: string;
  name: string;
  args?: unknown;
  result?: unknown;
  state: "pending" | "running" | "completed" | "failed";
  error?: string;
  meta?: MessageMeta;
}

/**
 * A reply from a sub-agent, injected into the parent transcript.
 * Distinct from `assistant` (the current persona's own turn) and
 * `user` (a local human turn) so orchestrator workflows can attribute
 * the message — `agent#7 said …` — instead of styling it like the
 * user typed it.
 */
export interface AgentMessage {
  kind: "agent";
  id?: string;
  agentId: string;
  text: string;
  /** False when the sub-agent terminated with `Failed` — drives error styling. */
  ok: boolean;
  meta?: MessageMeta;
}

export type ChatMessage =
  | UserMessage
  | AssistantMessage
  | SystemMessage
  | ThinkingMessage
  | ToolCallMessage
  | AgentMessage;

export type TurnState =
  | { kind: "idle" }
  | { kind: "streaming" }
  | { kind: "errored"; message: string };
