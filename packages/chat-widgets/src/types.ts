// Normalized data shapes the chat widgets render. Workflows adapt their
// protocol-specific snapshots into these and feed them to <ChatView />.

export type Role = "user" | "assistant" | "system";

export interface UserMessage {
  kind: "user";
  id?: string;
  text: string;
}

export interface AssistantMessage {
  kind: "assistant";
  id?: string;
  text: string;
  /** True when this is the in-flight message currently being streamed. */
  streaming?: boolean;
}

export interface SystemMessage {
  kind: "system";
  id?: string;
  text: string;
}

export interface ThinkingMessage {
  kind: "thinking";
  id?: string;
  text: string;
}

export interface ToolCallMessage {
  kind: "toolCall";
  id: string;
  name: string;
  args?: unknown;
  result?: unknown;
  state: "pending" | "running" | "completed" | "failed";
  error?: string;
}

export type ChatMessage =
  | UserMessage
  | AssistantMessage
  | SystemMessage
  | ThinkingMessage
  | ToolCallMessage;

export type TurnState =
  | { kind: "idle" }
  | { kind: "streaming" }
  | { kind: "errored"; message: string };
