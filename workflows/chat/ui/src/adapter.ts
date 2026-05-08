// Maps the chat workflow's protocol-shaped SessionSnapshot onto the
// generic shape @lutin/chat-widgets expects. Kept local because the
// snapshot shape is tied to this workflow's protocol; the widget
// package shouldn't know about it.

import type { ChatMessage, MessageMeta as WidgetMeta, TurnState } from "@lutin/chat-widgets";
import type { MessageMeta } from "./chat";
import type { Message, SessionSnapshot } from "./session";

export interface ChatViewModel {
  messages: ChatMessage[];
  turn: TurnState;
}

export function toViewModel(snap: SessionSnapshot): ChatViewModel {
  // Completed-message ids are the bare projected index as a string.
  // ChatView forwards them to MessageActions callbacks, which the
  // workflow parses back into an integer for the engine RPCs. Live /
  // flushed entries get non-numeric ids so context-menu callbacks can
  // ignore them — mutating an in-flight buffer would race the agent.
  const messages: ChatMessage[] = snap.completed.map((m, i) =>
    project(m, String(i), wireToWidgetMeta(snap.metrics[i])),
  );

  if (snap.turn.kind === "streaming") {
    snap.turn.flushed.forEach((m, i) => messages.push(project(m, `f${i}`, undefined)));
    if (snap.turn.stream === "assistant") {
      messages.push({ kind: "assistant", id: "live", text: snap.turn.buf, streaming: true });
    } else {
      messages.push({
        kind: "thinking",
        id: "live-thinking",
        text: snap.turn.buf,
        streaming: true,
      });
    }
    return { messages, turn: { kind: "streaming" } };
  }

  if (snap.turn.kind === "errored") {
    return { messages, turn: { kind: "errored", message: snap.turn.message } };
  }

  return { messages, turn: { kind: "idle" } };
}

function wireToWidgetMeta(m: MessageMeta | undefined): WidgetMeta | undefined {
  if (!m) return undefined;
  // Treat the empty timestamp as "no metrics yet" so the footer doesn't
  // render a half-blank row for transcripts loaded before metrics
  // existed.
  if (
    m.timestamp.length === 0 &&
    m.ttftMs === null &&
    m.durationMs === null &&
    m.promptTokens === null &&
    m.completionTokens === null
  ) {
    return undefined;
  }
  return {
    timestamp: m.timestamp,
    ttftMs: m.ttftMs === null ? null : Number(m.ttftMs),
    durationMs: m.durationMs === null ? null : Number(m.durationMs),
    promptTokens: m.promptTokens,
    completionTokens: m.completionTokens,
  };
}

function project(m: Message, id: string, meta: WidgetMeta | undefined): ChatMessage {
  switch (m.role) {
    case "user":
      return { kind: "user", id, text: m.text, meta };
    case "assistant":
      return { kind: "assistant", id, text: m.text, meta };
    case "thinking":
      return { kind: "thinking", id, text: m.text, meta };
    case "subAgentReply":
      return { kind: "agent", id, agentId: m.agentId, text: m.text, ok: true, meta };
    case "subAgentFailure":
      return {
        kind: "agent",
        id,
        agentId: m.agentId,
        text: m.reason,
        ok: false,
        meta,
      };
    case "tool": {
      // m.arguments is already parsed at the wire boundary (decode-time
      // JSON.parse in chat.ts). Just hand it through to the widget.
      const widgetState =
        m.status.kind === "ok"
          ? "completed"
          : m.status.kind === "failed"
            ? "failed"
            : "running";
      return {
        kind: "toolCall",
        id: m.callId,
        name: m.name,
        args: m.arguments,
        result: m.status.kind === "ok" ? m.status.output : undefined,
        state: widgetState,
        error: m.status.kind === "failed" ? m.status.reason : undefined,
        meta,
      };
    }
  }
}
