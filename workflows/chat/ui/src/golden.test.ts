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
} from "./chat";

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
          history: [{ role: "user", text: "hi" }],
        },
      },
    ],
    [
      "Err(NoTurnInFlight)",
      hex(0x01, 0x00),
      { ok: false, error: { kind: "noTurnInFlight" } },
    ],
  ];

  for (const [label, bytes, want] of cases) {
    test(label, () => {
      expect(decodeChatResponse(bytes)).toEqual(want);
    });
  }
});
