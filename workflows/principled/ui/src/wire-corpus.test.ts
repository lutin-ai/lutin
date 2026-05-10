// Exhaustive wire-codec round-trip. The Rust side generates
// `wire-corpus.json` (see `lib.rs::tests::wire_corpus_in_sync`) — one
// entry per variant of every wire-visible enum. This test decodes
// every entry and asserts the resulting `kind` matches the
// Rust-side tag. Drift in either direction (Rust adds a variant, TS
// reorders cases, a field type changes) fails this test instead of
// surfacing as a `postcard: unexpected EOF` in the chrome.

import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, test } from "bun:test";

import {
  decodeChatEvent,
  decodeChatResponse,
  encodeChatRequest,
  type ChatRequest,
} from "@lutin/principled-protocol";

interface CorpusEntry {
  name: string;
  channel: "request" | "response" | "event";
  kind: string;
  hex: string;
}

const corpus: CorpusEntry[] = JSON.parse(
  readFileSync(join(import.meta.dir, "..", "wire-corpus.json"), "utf8"),
);

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

describe("wire corpus", () => {
  for (const entry of corpus) {
    test(entry.name, () => {
      const bytes = fromHex(entry.hex);
      switch (entry.channel) {
        case "request": {
          // For requests TS encodes; round-trip by re-encoding a
          // representative value and matching golden bytes is already
          // covered by the static request goldens. Here we just
          // sanity-check that the corpus name maps to a known kind by
          // building a roundtrip via decode-equivalent: the bytes
          // already live in the JSON, so the meaningful check is that
          // the matching `ChatRequest` re-encodes to the same hex
          // (catches encoder drift). We can't re-derive a request
          // from bytes (no decoder), so we settle for asserting the
          // kind is in the known union.
          expect(KNOWN_REQUEST_KINDS).toContain(entry.kind);
          // Spot-check that re-encoding a minimal value of this kind
          // produces non-empty bytes — guards the encoder from
          // silently regressing to an empty buffer.
          const stub = STUB_REQUESTS[entry.kind as keyof typeof STUB_REQUESTS];
          expect(encodeChatRequest(stub).length).toBeGreaterThan(0);
          break;
        }
        case "response": {
          const resp = decodeChatResponse(bytes);
          if (resp.ok) expect(resp.value.kind).toBe(entry.kind);
          else expect(resp.error.kind).toBe(entry.kind);
          break;
        }
        case "event": {
          const ev = decodeChatEvent(bytes);
          expect(ev.kind).toBe(entry.kind);
          break;
        }
      }
    });
  }
});

const KNOWN_REQUEST_KINDS = [
  "subscribe",
  "sendMessage",
  "cancel",
  "setPersona",
  "getState",
  "listPersonas",
  "rerun",
  "editMessage",
  "deleteMessage",
  "deleteFromHere",
  "getMetrics",
  "listSubAgents",
  "getSubAgentTranscript",
  "listReviews",
] as const;

// Minimal stub for each request kind. Used only to guard the encoder
// against producing empty output — content doesn't have to match the
// Rust corpus byte-for-byte (the static goldens cover that).
const STUB_REQUESTS: Record<(typeof KNOWN_REQUEST_KINDS)[number], ChatRequest> = {
  subscribe: { kind: "subscribe" },
  sendMessage: { kind: "sendMessage", text: "" },
  cancel: { kind: "cancel" },
  setPersona: { kind: "setPersona", name: null },
  getState: { kind: "getState" },
  listPersonas: { kind: "listPersonas" },
  rerun: { kind: "rerun" },
  editMessage: { kind: "editMessage", index: 0, text: "" },
  deleteMessage: { kind: "deleteMessage", index: 0 },
  deleteFromHere: { kind: "deleteFromHere", index: 0 },
  getMetrics: { kind: "getMetrics" },
  listSubAgents: { kind: "listSubAgents" },
  getSubAgentTranscript: { kind: "getSubAgentTranscript", id: "" },
  listReviews: { kind: "listReviews" },
};
