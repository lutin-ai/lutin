import { useState } from "react";
import { useScrollStick } from "../hooks/useScrollStick";
import type { Slots } from "../slots";
import type { ChatMessage, TurnState } from "../types";
import {
  AssistantBubble,
  SystemBubble,
  UserBubble,
} from "./MessageBubble";
import { Composer as DefaultComposer } from "./Composer";
import { ErrorBanner as DefaultErrorBanner } from "./ErrorBanner";
import { Header as DefaultHeader } from "./Header";
import { Thinking as DefaultThinking } from "./Thinking";
import { ToolCall as DefaultToolCall } from "./ToolCall";

export interface ChatViewProps {
  messages: ChatMessage[];
  turn: TurnState;
  onSend: (text: string) => void;
  onCancel?: () => void;
  onApproveTool?: (id: string) => void;
  onDenyTool?: (id: string) => void;

  /** Composer placeholder. */
  placeholder?: string;
  /** Disable the composer (e.g. while disconnected). */
  inputDisabled?: boolean;
  /** Hide the composer entirely (read-only sessions). */
  hideComposer?: boolean;
  /** Controlled composer draft. When provided alongside
   * `onDraftChange`, ChatView delegates draft state to the parent —
   * lets workflows push text in from outside (e.g. transcription). */
  draft?: string;
  onDraftChange?: (text: string) => void;

  /** Per-component overrides; unspecified slots fall through to defaults. */
  slots?: Slots;

  /** Extra class on the root, for workflow-specific styling. */
  className?: string;
}

export function ChatView({
  messages,
  turn,
  onSend,
  onCancel,
  onApproveTool,
  onDenyTool,
  placeholder,
  inputDisabled = false,
  hideComposer = false,
  draft: draftProp,
  onDraftChange,
  slots,
  className,
}: ChatViewProps) {
  const [draftLocal, setDraftLocal] = useState("");
  const controlled = draftProp !== undefined && onDraftChange !== undefined;
  const draft = controlled ? draftProp! : draftLocal;
  const setDraft = controlled ? onDraftChange! : setDraftLocal;
  const scrollRef = useScrollStick([messages.length, lastTextLen(messages), turn]);

  const Header = slots?.Header ?? DefaultHeader;
  const ErrorBanner = slots?.ErrorBanner ?? DefaultErrorBanner;
  const Composer = slots?.Composer ?? DefaultComposer;
  const User = slots?.UserMessage ?? UserBubble;
  const Assistant = slots?.AssistantMessage ?? AssistantBubble;
  const System = slots?.SystemMessage ?? SystemBubble;
  const ThinkingC = slots?.Thinking ?? DefaultThinking;
  const ToolCallC = slots?.ToolCall ?? DefaultToolCall;

  const submit = () => {
    const text = draft.trim();
    if (text.length === 0) return;
    setDraft("");
    onSend(text);
  };

  const busy = turn.kind === "streaming";
  const errored = turn.kind === "errored" ? turn.message : null;

  const root = ["lutin-chat", className].filter(Boolean).join(" ");

  return (
    <div className={root}>
      <Header turn={turn} onCancel={onCancel} />
      {errored && <ErrorBanner message={errored} />}

      <div ref={scrollRef} className="lutin-chat__scrollback">
        {messages.map((m, i) => {
          const key = m.id ?? `${m.kind}-${i}`;
          switch (m.kind) {
            case "user":
              return <User key={key} message={m} />;
            case "assistant":
              return <Assistant key={key} message={m} />;
            case "system":
              return <System key={key} message={m} />;
            case "thinking":
              return <ThinkingC key={key} message={m} />;
            case "toolCall":
              return (
                <ToolCallC
                  key={key}
                  message={m}
                  onApprove={onApproveTool}
                  onDeny={onDenyTool}
                />
              );
          }
        })}
      </div>

      {!hideComposer && (
        <Composer
          value={draft}
          onChange={setDraft}
          onSubmit={submit}
          onCancel={onCancel}
          busy={busy}
          placeholder={placeholder}
          disabled={inputDisabled}
        />
      )}
    </div>
  );
}

// Cheap signal for "did the streaming buffer grow?" — feeds the
// scroll-stick hook so it reacts to in-flight token deltas, not just
// message boundaries.
function lastTextLen(messages: ChatMessage[]): number {
  const last = messages[messages.length - 1];
  if (!last) return 0;
  if ("text" in last && typeof last.text === "string") return last.text.length;
  return 0;
}
