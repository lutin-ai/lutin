// Public API. Workflows import from "@lutin/chat-widgets".

export type {
  AgentMessage,
  AssistantMessage,
  ChatMessage,
  MessageMeta,
  Role,
  SystemMessage,
  ThinkingMessage,
  ToolCallMessage,
  TurnState,
  UserMessage,
} from "./types";

export type {
  AgentMessageProps,
  AssistantMessageProps,
  ComposerProps,
  ErrorBannerProps,
  HeaderProps,
  Slots,
  SystemMessageProps,
  ThinkingProps,
  ToolCallProps,
  UserMessageProps,
} from "./slots";

export { ChatView } from "./components/ChatView";
export type { ChatViewProps } from "./components/ChatView";
export type { MessageActions } from "./components/MessageActions";

// Default presentational components, exported so workflows that
// compose primitives directly (without ChatView) can still use them.
export {
  AgentBubble,
  AssistantBubble,
  SystemBubble,
  UserBubble,
} from "./components/MessageBubble";
export { Composer } from "./components/Composer";
export { ErrorBanner } from "./components/ErrorBanner";
export { Header } from "./components/Header";
export { Thinking } from "./components/Thinking";
export { ToolCall } from "./components/ToolCall";

export { useScrollStick } from "./hooks/useScrollStick";

export { Markdown } from "./markdown";
export type { MarkdownProps } from "./markdown";
