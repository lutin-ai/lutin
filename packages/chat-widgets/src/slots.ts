import type { ComponentType } from "react";
import type { MessageActions } from "./components/MessageActions";
import type {
  AgentMessage,
  AssistantMessage,
  SystemMessage,
  ThinkingMessage,
  ToolCallMessage,
  TurnState,
  UserMessage,
} from "./types";

// Each slot has a single, stable props contract. Workflows can override
// any slot by passing a component with the same shape — no inheritance,
// no `children` plumbing, just drop-in replacement.

export interface UserMessageProps {
  message: UserMessage;
  actions?: MessageActions;
}
export interface AssistantMessageProps {
  message: AssistantMessage;
  actions?: MessageActions;
}
export interface SystemMessageProps {
  message: SystemMessage;
  actions?: MessageActions;
}
export interface ThinkingProps {
  message: ThinkingMessage;
  actions?: MessageActions;
}
export interface ToolCallProps {
  message: ToolCallMessage;
  onApprove?: (id: string) => void;
  onDeny?: (id: string) => void;
}
export interface AgentMessageProps {
  message: AgentMessage;
  actions?: MessageActions;
}
export interface ComposerProps {
  value: string;
  onChange: (v: string) => void;
  onSubmit: () => void;
  onCancel?: () => void;
  busy: boolean;
  placeholder?: string;
  disabled?: boolean;
}
export interface ErrorBannerProps {
  message: string;
  onDismiss?: () => void;
}
export interface HeaderProps {
  turn: TurnState;
  onCancel?: () => void;
}

export interface Slots {
  UserMessage?: ComponentType<UserMessageProps>;
  AssistantMessage?: ComponentType<AssistantMessageProps>;
  SystemMessage?: ComponentType<SystemMessageProps>;
  Thinking?: ComponentType<ThinkingProps>;
  ToolCall?: ComponentType<ToolCallProps>;
  AgentMessage?: ComponentType<AgentMessageProps>;
  Composer?: ComponentType<ComposerProps>;
  ErrorBanner?: ComponentType<ErrorBannerProps>;
  Header?: ComponentType<HeaderProps>;
}
