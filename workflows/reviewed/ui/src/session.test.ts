import { describe, expect, test } from "bun:test";
import { initialSnapshot, reduce, type Snapshot } from "./session";
import type { ChatEvent } from "@lutin/reviewed-protocol";

function ev(s: Snapshot, event: ChatEvent): Snapshot {
  return reduce(s, { type: "event", event });
}

describe("reviewed session", () => {
  test("principle verdicts key to the rendered tool-bubble id", () => {
    let s = reduce(initialSnapshot, { type: "optimistic", text: "hi" });
    s = ev(s, { kind: "userMessageAppended", id: "srv-1", text: "hi" });
    s = ev(s, { kind: "thinking", stepId: 0n, attempt: 0, text: "thinking…" });
    s = ev(s, {
      kind: "toolCallDrafted",
      stepId: 0n,
      attempt: 0,
      tool: "edit",
      args: { file_path: "a.ts" },
    });
    s = ev(s, {
      kind: "principleEvaluated",
      stepId: 0n,
      attempt: 0,
      principle: "naming-and-readability",
      verdict: { kind: "pass" },
    });
    s = ev(s, {
      kind: "principleSkipped",
      stepId: 0n,
      attempt: 0,
      principle: "shell-safety",
      streak: 5,
    });
    s = ev(s, {
      kind: "toolCallExecuted",
      stepId: 0n,
      tool: "edit",
      args: { file_path: "a.ts" },
      output: "done",
    });

    const tool = s.messages.find((m) => m.kind === "toolCall");
    expect(tool).toBeDefined();
    expect(tool!.id).toBe("1:0:0");
    if (tool!.kind === "toolCall") expect(tool!.state).toBe("completed");

    const checks = s.verdictsByCallId[tool!.id!];
    expect(checks).toBeDefined();
    expect(checks.map((c) => c.kind)).toEqual(["pass", "skipped"]);
  });
});
