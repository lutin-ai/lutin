// Maps the chat workflow's protocol-shaped SessionSnapshot onto the
// generic shape @lutin/chat-widgets expects. Kept local because the
// snapshot shape is tied to this workflow's protocol; the widget
// package shouldn't know about it.

import type { ChatMessage, TurnState } from "@lutin/chat-widgets";
import type { Message, SessionSnapshot } from "./session";

export interface ChatViewModel {
  messages: ChatMessage[];
  turn: TurnState;
}

export function toViewModel(snap: SessionSnapshot): ChatViewModel {
  const messages: ChatMessage[] = snap.completed.map((m, i) => project(m, `c${i}`));

  if (snap.turn.kind === "streaming") {
    snap.turn.flushed.forEach((m, i) => messages.push(project(m, `f${i}`)));
    if (snap.turn.stream === "assistant") {
      messages.push({ kind: "assistant", id: "live", text: snap.turn.buf, streaming: true });
    } else {
      messages.push({ kind: "thinking", id: "live-thinking", text: snap.turn.buf });
    }
    return { messages, turn: { kind: "streaming" } };
  }

  if (snap.turn.kind === "errored") {
    return { messages, turn: { kind: "errored", message: snap.turn.message } };
  }

  return { messages, turn: { kind: "idle" } };
}

function project(m: Message, id: string): ChatMessage {
  switch (m.role) {
    case "user":
      return { kind: "user", id, text: m.text };
    case "assistant":
      return { kind: "assistant", id, text: m.text };
    case "thinking":
      return { kind: "thinking", id, text: m.text };
  }
}
