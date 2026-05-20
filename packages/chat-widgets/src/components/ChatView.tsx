import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useScrollStick } from "../hooks/useScrollStick";
import type { Slots } from "../slots";
import type { ChatMessage, TurnState } from "../types";
import type { MessageActions } from "./MessageActions";
import {
  AgentBubble,
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

  /** Per-message right-click affordances. Forwarded to every bubble;
   *  bubbles render the menu only for messages with stable ids. */
  messageActions?: MessageActions;

  /** Per-component overrides; unspecified slots fall through to defaults. */
  slots?: Slots;

  /** Extra class on the root, for workflow-specific styling. */
  className?: string;
}

// Rough first-paint estimate before the virtualizer measures real rows.
// Tuned to a typical assistant bubble; over- or under-shooting just
// means the scroll thumb settles after first measure.
const EST_ROW_PX = 96;

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
  messageActions,
  slots,
  className,
}: ChatViewProps) {
  const [draftLocal, setDraftLocal] = useState("");
  const controlled = draftProp !== undefined && onDraftChange !== undefined;
  const draft = controlled ? draftProp! : draftLocal;
  const setDraft = controlled ? onDraftChange! : setDraftLocal;

  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: messages.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => EST_ROW_PX,
    overscan: 6,
    getItemKey: (i) => messages[i].id ?? `${messages[i].kind}-${i}`,
  });
  const totalSize = virtualizer.getTotalSize();
  // Re-stick when the virtualizer remeasures rows (mid-stream height
  // changes) as well as when the message list itself grows.
  // Bottom padding so the latest user message can anchor at the top of
  // the viewport even when the agent reply is short or empty. Tracks
  // the scroll container's clientHeight so it stays correct across
  // resizes — without this the virtualizer caps total height at the
  // measured content and `anchorAt` is silently clamped.
  const [bottomPad, setBottomPad] = useState(0);
  // Read via ref so the scroll hook always sees the latest value
  // without making `bottomPad` a dependency of its layout effect
  // (which would re-snap on every resize).
  const bottomPadRef = useRef(0);
  bottomPadRef.current = bottomPad;
  const { anchorAt, scrollToBottom, stuck } = useScrollStick(
    scrollRef,
    [messages.length, lastTextLen(messages), turn, totalSize],
    () => bottomPadRef.current,
  );

  // "Jump to latest" pill: counts messages that landed after the user
  // scrolled away. Resets to zero on re-stick. Stored as a length
  // snapshot (not a count) so we don't have to subscribe to every
  // streaming-token render.
  const [unstuckAtLen, setUnstuckAtLen] = useState<number | null>(null);
  useEffect(() => {
    if (stuck) setUnstuckAtLen(null);
    else if (unstuckAtLen === null) setUnstuckAtLen(messages.length);
  }, [stuck, messages.length, unstuckAtLen]);
  const pendingCount = unstuckAtLen === null ? 0 : Math.max(0, messages.length - unstuckAtLen);
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const update = () => setBottomPad(el.clientHeight);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // After a new user message is appended, scroll so that message sits
  // at the top of the viewport. Tracked by id (not index) so we don't
  // re-anchor when an old user message gets re-rendered mid-stream.
  const lastUserIdRef = useRef<string | null>(null);
  useLayoutEffect(() => {
    let idx = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].kind === "user") {
        idx = i;
        break;
      }
    }
    if (idx < 0) return;
    const id = messages[idx].id ?? null;
    if (id === null || id === lastUserIdRef.current) return;
    lastUserIdRef.current = id;
    const offset = virtualizer.getVirtualItems().find((vi) => vi.index === idx)?.start;
    if (offset !== undefined) anchorAt(offset);
    else {
      // Row not in the virtual window yet — ask the virtualizer to
      // bring it on, then anchor on the next layout pass via the
      // measured offset.
      virtualizer.scrollToIndex(idx, { align: "start" });
    }
  }, [messages, virtualizer, anchorAt]);

  const Header = slots?.Header ?? DefaultHeader;
  const ErrorBanner = slots?.ErrorBanner ?? DefaultErrorBanner;
  const Composer = slots?.Composer ?? DefaultComposer;
  const User = slots?.UserMessage ?? UserBubble;
  const Assistant = slots?.AssistantMessage ?? AssistantBubble;
  const System = slots?.SystemMessage ?? SystemBubble;
  const ThinkingC = slots?.Thinking ?? DefaultThinking;
  const ToolCallC = slots?.ToolCall ?? DefaultToolCall;
  const Agent = slots?.AgentMessage ?? AgentBubble;

  const submit = () => {
    const text = draft.trim();
    if (text.length === 0) return;
    setDraft("");
    onSend(text);
  };

  const busy = turn.kind === "streaming";
  const errored =
    turn.kind === "errored"
      ? turn.message
      : turn.kind === "maxRounds"
        ? "Reached max rounds — agent paused. Send a message to continue."
        : null;

  const root = ["lutin-chat", className].filter(Boolean).join(" ");

  return (
    <div className={root}>
      <Header turn={turn} onCancel={onCancel} />
      {errored && <ErrorBanner message={errored} />}

      <div className="lutin-chat__scroll-wrap">
      <div ref={scrollRef} className="lutin-chat__scrollback">
        <div className="lutin-chat__virt" style={{ height: totalSize + bottomPad }}>
          {virtualizer.getVirtualItems().map((vi) => {
            const m = messages[vi.index];
            return (
              <div
                key={vi.key}
                data-index={vi.index}
                ref={virtualizer.measureElement}
                className="lutin-chat__vrow"
                style={{ transform: `translateY(${vi.start}px)` }}
              >
                {renderMessage(m, {
                  User,
                  Assistant,
                  System,
                  ThinkingC,
                  ToolCallC,
                  Agent,
                  messageActions,
                  onApproveTool,
                  onDenyTool,
                })}
              </div>
            );
          })}
        </div>
      </div>

      {!stuck && (
        <button
          type="button"
          className="lutin-chat__jump-pill"
          onClick={scrollToBottom}
          aria-label="Jump to latest"
        >
          {pendingCount > 0 ? `${pendingCount} new` : "Jump to latest"}
          <span aria-hidden="true"> ↓</span>
        </button>
      )}
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

interface RenderCtx {
  User: NonNullable<Slots["UserMessage"]> | typeof UserBubble;
  Assistant: NonNullable<Slots["AssistantMessage"]> | typeof AssistantBubble;
  System: NonNullable<Slots["SystemMessage"]> | typeof SystemBubble;
  ThinkingC: NonNullable<Slots["Thinking"]> | typeof DefaultThinking;
  ToolCallC: NonNullable<Slots["ToolCall"]> | typeof DefaultToolCall;
  Agent: NonNullable<Slots["AgentMessage"]> | typeof AgentBubble;
  messageActions?: MessageActions;
  onApproveTool?: (id: string) => void;
  onDenyTool?: (id: string) => void;
}

function renderMessage(m: ChatMessage, ctx: RenderCtx) {
  const { User, Assistant, System, ThinkingC, ToolCallC, Agent } = ctx;
  switch (m.kind) {
    case "user":
      return <User message={m} actions={ctx.messageActions} />;
    case "assistant":
      return <Assistant message={m} actions={ctx.messageActions} />;
    case "system":
      return <System message={m} actions={ctx.messageActions} />;
    case "thinking":
      return <ThinkingC message={m} actions={ctx.messageActions} />;
    case "toolCall":
      return (
        <ToolCallC
          message={m}
          onApprove={ctx.onApproveTool}
          onDeny={ctx.onDenyTool}
        />
      );
    case "agent":
      return <Agent message={m} actions={ctx.messageActions} />;
  }
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
