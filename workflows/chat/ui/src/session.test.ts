// Reducer: multiple tool calls in one assistant turn must each become a
// distinct message entry. Regression test for "agent#N counter shows N>1
// even though only one bubble is visible" — the engine reports two
// separate calls but the UI was suspected of collapsing them.

import { describe, expect, test } from "bun:test";
import { initialSnapshot, reduce } from "./session";
import { toViewModel } from "./adapter";

describe("two tool calls in one turn", () => {
  test("both appear in flushed during streaming", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = reduce(s, {
      type: "event",
      event: { kind: "delta", text: "\n\n" },
    });
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallStarted",
        id: "call_a",
        name: "spawn_agent",
        arguments: { initial_prompt: "first" },
      },
    });
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallStarted",
        id: "call_b",
        name: "spawn_agent",
        arguments: { initial_prompt: "second" },
      },
    });
    expect(s.turn.kind).toBe("streaming");
    if (s.turn.kind !== "streaming") throw new Error("unreachable");
    const tools = s.turn.flushed.filter((m) => m.role === "tool");
    expect(tools.length).toBe(2);
    expect(tools.map((t) => (t as any).callId)).toEqual(["call_a", "call_b"]);
  });

  test("toViewModel emits two toolCall messages with distinct ids", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallStarted",
        id: "call_a",
        name: "spawn_agent",
        arguments: {},
      },
    });
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallStarted",
        id: "call_b",
        name: "spawn_agent",
        arguments: {},
      },
    });
    const vm = toViewModel(s);
    const tools = vm.messages.filter((m) => m.kind === "toolCall");
    expect(tools.length).toBe(2);
    expect(tools.map((t) => t.id)).toEqual(["call_a", "call_b"]);
  });

  test("two-call flow with same id collapses to one (would explain the bug)", () => {
    let s = initialSnapshot;
    s = reduce(s, { type: "submitOptimistic", text: "go" });
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallStarted",
        id: "dup",
        name: "spawn_agent",
        arguments: {},
      },
    });
    s = reduce(s, {
      type: "event",
      event: {
        kind: "toolCallStarted",
        id: "dup",
        name: "spawn_agent",
        arguments: {},
      },
    });
    if (s.turn.kind !== "streaming") throw new Error("unreachable");
    const tools = s.turn.flushed.filter((m) => m.role === "tool");
    // Today the reducer keeps both even on duplicate id, but React's
    // key={m.id} would drop one. This test pins current reducer
    // behavior; the visible-bug path is in ChatView's keying.
    expect(tools.length).toBe(2);
  });
});
