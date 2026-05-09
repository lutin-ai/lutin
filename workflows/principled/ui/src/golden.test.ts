// Golden bytes pinned against the Rust side in
// `workflows/chat/src/lib.rs::tests::golden_postcard_bytes`. Any change
// here is a wire-format change; mirror it on the Rust side too.
//
// Run with `bun test`.

import { describe, expect, test } from "bun:test";

import {
  type ChatEvent,
  type ChatRequest,
  type ChatResponse,
  decodeChatEvent,
  decodeChatResponse,
  encodeChatRequest,
} from "@lutin/principled-protocol";

const hex = (...bs: number[]) => new Uint8Array(bs);
const eqBytes = (a: Uint8Array, b: Uint8Array) =>
  a.length === b.length && a.every((v, i) => v === b[i]);

describe("ChatRequest encode goldens", () => {
  const cases: Array<[string, ChatRequest, Uint8Array]> = [
    ["Subscribe", { kind: "subscribe" }, hex(0x00)],
    [
      "SendMessage{hi}",
      { kind: "sendMessage", text: "hi" },
      hex(0x01, 0x02, 0x68, 0x69),
    ],
    ["Cancel", { kind: "cancel" }, hex(0x02)],
    ["SetPersona(None)", { kind: "setPersona", name: null }, hex(0x03, 0x00)],
    [
      "SetPersona(alice)",
      { kind: "setPersona", name: "alice" },
      hex(0x03, 0x01, 0x05, 0x61, 0x6c, 0x69, 0x63, 0x65),
    ],
    ["ListPersonas", { kind: "listPersonas" }, hex(0x05)],
    ["Rerun", { kind: "rerun" }, hex(0x06)],
    [
      "EditMessage{3,hi}",
      { kind: "editMessage", index: 3, text: "hi" },
      hex(0x07, 0x03, 0x02, 0x68, 0x69),
    ],
    ["DeleteMessage{2}", { kind: "deleteMessage", index: 2 }, hex(0x08, 0x02)],
    ["DeleteFromHere{1}", { kind: "deleteFromHere", index: 1 }, hex(0x09, 0x01)],
    ["GetMetrics", { kind: "getMetrics" }, hex(0x0a)],
    ["ListReviews", { kind: "listReviews" }, hex(0x0d)],
  ];

  for (const [label, req, want] of cases) {
    test(label, () => {
      const got = encodeChatRequest(req);
      expect(eqBytes(got, want)).toBe(true);
    });
  }
});

describe("ChatEvent decode goldens", () => {
  const cases: Array<[string, Uint8Array, ChatEvent]> = [
    ["Delta(hi)", hex(0x00, 0x02, 0x68, 0x69), { kind: "delta", text: "hi" }],
    [
      "MessageFinished{7, Completed}",
      hex(0x04, 0x07, 0x00),
      {
        kind: "messageFinished",
        turnId: 7n,
        reason: { kind: "completed" },
      },
    ],
    [
      "MessageFinished{300, Failed(boom)}",
      hex(0x04, 0xac, 0x02, 0x02, 0x04, 0x62, 0x6f, 0x6f, 0x6d),
      {
        kind: "messageFinished",
        turnId: 300n,
        reason: { kind: "failed", message: "boom" },
      },
    ],
    [
      "HistoryReplaced(empty)",
      hex(0x06, 0x00),
      { kind: "historyReplaced", history: [] },
    ],
    [
      "MetricsReplaced(empty)",
      hex(0x07, 0x00),
      { kind: "metricsReplaced", metrics: [] },
    ],
    [
      "ReviewFrameOpened",
      hex(0x0a, 0x07, 0x04, 0x65, 0x64, 0x69, 0x74, 0x00),
      {
        kind: "reviewFrameOpened",
        stepId: 7n,
        toolName: "edit",
        argsSummary: "",
      },
    ],
    [
      "ReviewerStarted",
      hex(0x0b, 0x07, 0x01, 0x01, 0x70),
      {
        kind: "reviewerStarted",
        stepId: 7n,
        reviewerCallId: 1n,
        principle: "p",
      },
    ],
    [
      "ReviewerCompleted(Pass)",
      hex(0x0c, 0x07, 0x01, 0x01, 0x70, 0x00, 0x01, 0x54),
      {
        kind: "reviewerCompleted",
        stepId: 7n,
        reviewerCallId: 1n,
        principle: "p",
        verdict: { kind: "pass" },
        ts: "T",
      },
    ],
    [
      "ReviewerCompleted(Fail{Fix})",
      hex(
        0x0c,
        0x07, 0x02,
        0x01, 0x70,
        0x02,
        0x00,
        0x02, 0x6e, 0x6f,
        0x00,
        0x01, 0x54,
      ),
      {
        kind: "reviewerCompleted",
        stepId: 7n,
        reviewerCallId: 2n,
        principle: "p",
        verdict: {
          kind: "fail",
          severity: { kind: "fix" },
          reasoning: "no",
          suggestedFix: null,
        },
        ts: "T",
      },
    ],
    [
      "ReviewFrameProgress",
      hex(0x0d, 0x07, 0x01, 0x03, 0x01, 0x01, 0x70),
      {
        kind: "reviewFrameProgress",
        stepId: 7n,
        attempt: 1,
        maxAttempts: 3,
        blocking: ["p"],
      },
    ],
    [
      "ReviewFrameResolved(Accepted)",
      hex(0x0e, 0x07, 0x00),
      {
        kind: "reviewFrameResolved",
        stepId: 7n,
        outcome: { kind: "accepted" },
      },
    ],
    [
      "ReviewFrameResolved(Rewound)",
      hex(0x0e, 0x07, 0x01, 0x02, 0x66, 0x62),
      {
        kind: "reviewFrameResolved",
        stepId: 7n,
        outcome: { kind: "rewound", feedback: "fb" },
      },
    ],
  ];

  for (const [label, bytes, want] of cases) {
    test(label, () => {
      expect(decodeChatEvent(bytes)).toEqual(want);
    });
  }
});

describe("ChatResponse decode goldens", () => {
  const cases: Array<[string, Uint8Array, ChatResponse]> = [
    [
      "Ok(Subscribed{empty})",
      hex(0x00, 0x00, 0x00, 0x00, 0x00),
      {
        ok: true,
        value: {
          kind: "subscribed",
          state: { persona: null, modelOverride: null },
          history: [],
        },
      },
    ],
    [
      "Ok(Subscribed{persona,1msg})",
      hex(
        0x00, 0x00,
        0x01, 0x05, 0x61, 0x6c, 0x69, 0x63, 0x65,
        0x00,
        0x01,
        0x00,
        0x02, 0x68, 0x69,
      ),
      {
        ok: true,
        value: {
          kind: "subscribed",
          state: { persona: "alice", modelOverride: null },
          history: [{ kind: "user", text: "hi" }],
        },
      },
    ],
    [
      "Err(NoTurnInFlight)",
      hex(0x01, 0x00),
      { ok: false, error: { kind: "noTurnInFlight" } },
    ],
    [
      "Ok(Metrics(empty))",
      hex(0x00, 0x07, 0x00),
      { ok: true, value: { kind: "metrics", metrics: [] } },
    ],
    [
      "Ok(Metrics(1user))",
      hex(0x00, 0x07, 0x01, 0x00, 0x01, 0x01, 0x54),
      {
        ok: true,
        value: {
          kind: "metrics",
          metrics: [{ kind: "user", timestamp: "T" }],
        },
      },
    ],
    [
      "Ok(Reviews(empty))",
      hex(0x00, 0x0a, 0x00),
      { ok: true, value: { kind: "reviews", reviews: [] } },
    ],
    [
      "Ok(Reviews(1row))",
      hex(
        0x00, 0x0a,
        0x01,
        0x01, 0x54,
        0x07,
        0x01,
        0x01, 0x70,
        0x01, // persona Option tag = Some
        0x01, 0x72,
        0x04, 0x65, 0x64, 0x69, 0x74,
        0x00,
        0x00,
      ),
      {
        ok: true,
        value: {
          kind: "reviews",
          reviews: [
            {
              ts: "T",
              stepId: 7n,
              reviewerCallId: 1n,
              principle: "p",
              persona: "r",
              toolName: "edit",
              argsSummary: "",
              verdict: { kind: "pass" },
            },
          ],
        },
      },
    ],
  ];

  for (const [label, bytes, want] of cases) {
    test(label, () => {
      expect(decodeChatResponse(bytes)).toEqual(want);
    });
  }
});
