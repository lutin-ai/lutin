// Maps the chat workflow's protocol-shaped SessionSnapshot onto the
// generic shape @lutin/chat-widgets expects. Kept local because the
// snapshot shape is tied to this workflow's protocol; the widget
// package shouldn't know about it.

import type { ChatMessage, MessageMeta as WidgetMeta, TurnState } from "@lutin/chat-widgets";
import type { MessageMeta } from "@lutin/principled-protocol";
import type { ActiveReview, Message, SessionSnapshot } from "./session";

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
    appendActiveReviews(messages, snap);
    return { messages, turn: { kind: "streaming" } };
  }

  if (snap.turn.kind === "errored") {
    appendActiveReviews(messages, snap);
    return { messages, turn: { kind: "errored", message: snap.turn.message } };
  }

  if (snap.turn.kind === "maxRounds") {
    appendActiveReviews(messages, snap);
    return { messages, turn: { kind: "maxRounds" } };
  }

  appendActiveReviews(messages, snap);
  return { messages, turn: { kind: "idle" } };
}

/// Render in-flight review frames as inline system placeholders. Each
/// frame collapses to one line: "reviewing edit foo.rs · attempt 2/3
/// · 1 principle blocking". On `reviewFrameResolved` the frame leaves
/// `activeReviews` and the placeholder disappears — for accepted
/// steps the real `toolCall` already streamed in via the SDK, taking
/// its scrollback slot.
function appendActiveReviews(out: ChatMessage[], snap: SessionSnapshot): void {
  for (const frame of snap.activeReviews) {
    out.push({
      kind: "system",
      id: `review-${frame.stepId}`,
      text: reviewLine(frame),
    });
  }
}

function reviewLine(frame: ActiveReview): string {
  const args = frame.argsSummary ? `(${frame.argsSummary})` : "";
  const head = `reviewing ${frame.toolName}${args}`;
  // Pre-progress: only the first attempt is in flight and the
  // engine hasn't told us a budget yet (per-principle, only known
  // on retry). Render "attempt 1" without a denominator so the
  // chrome doesn't fabricate one.
  const attempt =
    frame.progress === null
      ? "attempt 1"
      : `attempt ${frame.progress.attempt}/${frame.progress.maxAttempts}`;
  const blocking =
    frame.blocking.length === 0
      ? "all reviewers running…"
      : `${frame.blocking.length} principle${frame.blocking.length === 1 ? "" : "s"} blocking: ${frame.blocking.join(", ")}`;
  return `${head} · ${attempt} · ${blocking}`;
}

function wireToWidgetMeta(m: MessageMeta | undefined): WidgetMeta | undefined {
  if (!m) return undefined;
  // Parse RFC3339 once at the wire boundary so the widget doesn't
  // re-parse on every render. `null` means "no timestamp recorded"
  // (legacy transcript pre-metrics).
  const time = parseTimestamp(m.timestamp);
  switch (m.kind) {
    case "user":
    case "subAgentReply":
    case "subAgentFailure":
      return time === null ? undefined : { time };
    case "tool":
      if (time === null && m.durationMs === null) return undefined;
      return { time, durationMs: m.durationMs };
    case "thinking":
      if (time === null && m.ttftMs === null && m.durationMs === null) {
        return undefined;
      }
      return { time, ttftMs: m.ttftMs, durationMs: m.durationMs };
    case "assistant":
      if (
        time === null &&
        m.ttftMs === null &&
        m.durationMs === null &&
        m.promptTokens === null &&
        m.completionTokens === null
      ) {
        return undefined;
      }
      return {
        time,
        ttftMs: m.ttftMs,
        durationMs: m.durationMs,
        promptTokens: m.promptTokens,
        completionTokens: m.completionTokens,
      };
  }
}

/// Render-only sibling of [`toViewModel`] for sub-agent transcripts.
/// They never stream live in this UI (the parent owns the agent
/// stream), have no metrics sidecar, and are never user-edited — so we
/// project a flat list with stable `sub-N` ids and no widget meta.
export function subAgentViewModel(messages: Message[]): ChatViewModel {
  const out: ChatMessage[] = messages.map((m, i) => project(m, `sub-${i}`, undefined));
  return { messages: out, turn: { kind: "idle" } };
}

function parseTimestamp(ts: string | null): Date | null {
  if (ts === null) return null;
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? null : d;
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
        args: m.args,
        result: m.status.kind === "ok" ? m.status.output : undefined,
        state: widgetState,
        error: m.status.kind === "failed" ? m.status.reason : undefined,
        meta,
      };
    }
  }
}
