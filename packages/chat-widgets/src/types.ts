// Normalized data shapes the chat widgets render. Workflows adapt their
// protocol-specific snapshots into these and feed them to <ChatView />.

export type Role = "user" | "assistant" | "system";

/** Per-message metrics rendered as a small footer. All numeric fields
 *  are nullable because not every message has every metric (a user
 *  message has only a timestamp; a tool call has timestamp + duration;
 *  an assistant text has the full set). `timestamp` is RFC3339; an
 *  empty string suppresses the time chip. */
export interface MessageMeta {
  timestamp: string;
  ttftMs: number | null;
  durationMs: number | null;
  promptTokens: number | null;
  completionTokens: number | null;
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
