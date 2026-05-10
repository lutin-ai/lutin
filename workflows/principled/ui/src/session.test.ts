// Reducer: multiple tool calls in one assistant turn must each become a
// distinct message entry. Regression test for "agent#N counter shows N>1
// even though only one bubble is visible" — the engine reports two
// separate calls but the UI was suspected of collapsing them.

import { describe, expect, test } from "bun:test";
import { initialSnapshot, reduce, type SessionSnapshot } from "./session";
import { toViewModel } from "./adapter";

// Helper: send the streaming-start + parsed-args pair the SDK
// guarantees in order, so each test reads as the natural lifecycle
// rather than poking the parsed event in isolation.
function emitToolCall(
  s: SessionSnapshot,
  id: string,
  name: string,
  args: unknown,
): SessionSnapshot {
  s = reduce(s, {
    type: "event",
    event: { kind: "toolCallStreaming", id, name },
  });
  return reduce(s, {
    type: "event",
    event: { kind: "toolCallArgsParsed", id, name, arguments: args },
  });
}

describe("two tool calls in one turn", () => {
  test("both appear in flushed during streaming", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = reduce(s, {
      type: "event",
      event: { kind: "delta", text: "\n\n" },
    });
    s = emitToolCall(s, "call_a", "spawn_agent", { initial_prompt: "first" });
    s = emitToolCall(s, "call_b", "spawn_agent", { initial_prompt: "second" });
    expect(s.turn.kind).toBe("streaming");
    if (s.turn.kind !== "streaming") throw new Error("unreachable");
    const tools = s.turn.flushed.filter((m) => m.role === "tool");
    expect(tools.length).toBe(2);
    expect(tools.map((t) => (t as any).callId)).toEqual(["call_a", "call_b"]);
  });

  test("toViewModel emits two toolCall messages with distinct ids", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = emitToolCall(s, "call_a", "spawn_agent", {});
    s = emitToolCall(s, "call_b", "spawn_agent", {});
    const vm = toViewModel(s);
    const tools = vm.messages.filter((m) => m.kind === "toolCall");
    expect(tools.length).toBe(2);
    expect(tools.map((t) => t.id)).toEqual(["call_a", "call_b"]);
  });

  test("duplicate streaming-start appends a second entry (SDK invariant: unique ids)", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = emitToolCall(s, "dup", "spawn_agent", {});
    s = emitToolCall(s, "dup", "spawn_agent", {});
    if (s.turn.kind !== "streaming") throw new Error("unreachable");
    const tools = s.turn.flushed.filter((m) => m.role === "tool");
    // The agent SDK guarantees unique callIds per session, so this
    // path shouldn't fire in production. The reducer doesn't dedupe —
    // if upstream ever violates the invariant, both entries land in
    // `flushed` and React's `key={m.id}` drops the visual duplicate.
    // Pinned here so a future "dedupe in pushToolStart" change is
    // visible as a behavior shift, not silent.
    expect(tools.length).toBe(2);
  });

  test("streaming → delta → parsed lifecycle fills args in order", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = reduce(s, {
      type: "event",
      event: { kind: "toolCallStreaming", id: "call_x", name: "fetch" },
    });
    if (s.turn.kind !== "streaming") throw new Error("unreachable");
    {
      const tool = s.turn.flushed.find(
        (m) => m.role === "tool" && m.callId === "call_x",
      );
      if (!tool || tool.role !== "tool") throw new Error("entry missing");
      expect(tool.args.kind).toBe("streaming");
    }
    s = reduce(s, {
      type: "event",
      event: { kind: "toolCallArgsDelta", id: "call_x", args: '{"u":' },
    });
    s = reduce(s, {
      type: "event",
      event: { kind: "toolCallArgsDelta", id: "call_x", args: '"x"}' },
    });
    if (s.turn.kind !== "streaming") throw new Error("unreachable");
    {
      const tool = s.turn.flushed.find(
        (m) => m.role === "tool" && m.callId === "call_x",
      );
      if (!tool || tool.role !== "tool" || tool.args.kind !== "streaming") {
        throw new Error("expected streaming args");
      }
      expect(tool.args.raw).toBe('{"u":"x"}');
    }
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallArgsParsed",
        id: "call_x",
        name: "fetch",
        arguments: { u: "x" },
      },
    });
    if (s.turn.kind !== "streaming") throw new Error("unreachable");
    const tool = s.turn.flushed.find(
      (m) => m.role === "tool" && m.callId === "call_x",
    );
    if (!tool || tool.role !== "tool" || tool.args.kind !== "parsed") {
      throw new Error("expected parsed args");
    }
    expect(tool.args.value).toEqual({ u: "x" });
  });
});

describe("historyReplaced mid-stream (rewind)", () => {
  // A reviewer-driven rewind cancels the agent without emitting
  // `toolCallCompleted` for the in-flight tool, then emits a
  // `historyReplaced` reflecting the truncated transcript. Without
  // resetting `turn.flushed`, the orphaned running tool entry would
  // ride along to the next `messageFinished` and end up permanently
  // glued into `completed` with `status: running`.
  test("clears in-flight flushed/buf so orphaned running tools don't leak", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = reduce(s, { type: "event", event: { kind: "delta", text: "thinking..." } });
    s = emitToolCall(s, "call_rewound", "fs_write", { path: "x" });
    if (s.turn.kind !== "streaming") throw new Error("expected streaming");
    // pushToolStart flushed the prior text buf, so flushed = [text, tool].
    expect(s.turn.flushed.length).toBe(2);
    expect(
      s.turn.flushed.some((m) => m.role === "tool" && m.status.kind === "running"),
    ).toBe(true);

    // Rewind: engine emits new history without the in-flight tool.
    s = reduce(s, {
      type: "event",
      event: { kind: "historyReplaced", history: [{ kind: "user", text: "go" }] },
    });
    if (s.turn.kind !== "streaming") throw new Error("turn should still be streaming");
    expect(s.turn.flushed).toEqual([]);
    expect(s.turn.buf).toBe("");

    // Post-rewind stream proceeds; messageFinished should not resurrect
    // the rewound tool.
    s = reduce(s, { type: "event", event: { kind: "delta", text: "retry" } });
    s = reduce(s, {
      type: "event",
      event: { kind: "messageFinished", turnId: 1n, reason: { kind: "completed" } },
    });
    const stuck = s.completed.filter(
      (m) => m.role === "tool" && m.status.kind === "running",
    );
    expect(stuck).toEqual([]);
  });

  test("idle turn: historyReplaced just swaps completed", () => {
    const s = reduce(initialSnapshot, {
      type: "event",
      event: { kind: "historyReplaced", history: [{ kind: "user", text: "hi" }] },
    });
    expect(s.turn.kind).toBe("idle");
    expect(s.completed).toEqual([{ role: "user", text: "hi" }]);
  });
});

describe("attemptsSquashed (live denied-attempt cleanup)", () => {
  // Engine-emitted event: when a step accepts (or rewinds), the failed
  // attempts are no longer in the canonical transcript, so the UI
  // drops their tool bubbles before end-of-turn historyReplaced lands.
  test("drops streaming flushed entries by callId", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = emitToolCall(s, "denied_a", "fs_write", { v: 1 });
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallCompleted",
        id: "denied_a",
        outcome: { kind: "failed", text: "rejected" },
      },
    });
    s = emitToolCall(s, "accepted_b", "fs_write", { v: 2 });
    if (s.turn.kind !== "streaming") throw new Error("expected streaming");
    expect(s.turn.flushed.filter((m) => m.role === "tool").length).toBe(2);

    s = reduce(s, {
      type: "event",
      event: { kind: "attemptsSquashed", callIds: ["denied_a"] },
    });
    if (s.turn.kind !== "streaming") throw new Error("still streaming");
    const tools = s.turn.flushed.filter((m) => m.role === "tool");
    expect(tools.length).toBe(1);
    expect(tools[0]?.role === "tool" && tools[0].callId).toBe("accepted_b");
  });

  test("drops completed entries by callId when turn is idle", () => {
    let s: SessionSnapshot = {
      ...initialSnapshot,
      completed: [
        { role: "user", text: "go" },
        {
          role: "tool",
          callId: "denied_x",
          name: "fs_write",
          args: { kind: "parsed", value: {} },
          status: { kind: "failed", reason: "denied" },
        },
        { role: "assistant", text: "trying again" },
      ],
    };
    s = reduce(s, {
      type: "event",
      event: { kind: "attemptsSquashed", callIds: ["denied_x"] },
    });
    expect(s.completed.some((m) => m.role === "tool")).toBe(false);
    expect(s.completed.length).toBe(2);
  });
});
