// Shared wire-shape types for the chrome ↔ workflow boundary.
//
// Both chrome (`lutin-desktop/src/types.ts`) and any workflow that
// talks TTS need exact-matching enum strings; duplicating them in two
// trees made a new voice a three-edit operation with no drift signal.
// This package is the single source of truth for the JS side; the
// authoritative source is still Rust (`lutin-control-protocol`).

/// Mirrors Rust `OrpheusModel`. CP holds the variant → URL/filename
/// mapping; the wire surface stays opaque.
export type OrpheusModel = "ThreeBQ4KM";

/// Mirrors Rust `OrpheusVoice`. Closed enum so the wire surface can't
/// smuggle arbitrary strings into the model's prompt template.
export type OrpheusVoice =
  | "Tara"
  | "Leah"
  | "Jess"
  | "Leo"
  | "Dan"
  | "Mia"
  | "Zac"
  | "Zoe";

/// Mirrors Rust `TtsBackend` — externally tagged on the variant name.
export type TtsBackend = {
  Orpheus: { model: OrpheusModel; voice: OrpheusVoice };
};

/// `TtsStreamId(u32)` serializes as a bare number (newtype struct).
export type TtsStreamId = number;

/// One row in a workflow's sub-agent hierarchy. Today this is a
/// chat-specific concept (the chat workflow declares the
/// `sub_agents` capability and pushes its registry up via
/// `lutin.publishSubAgents`); the shape lives here so chrome and
/// the workflow side share a single TS definition. Authoritative
/// source is Rust `chat::SubAgentInfo`.
export type SubAgentStatus =
  | { kind: "running" }
  | { kind: "completed" }
  | { kind: "failed"; reason: string }
  | { kind: "stopped" };

export interface SubAgentRow {
  id: string;
  /// `null` for top-level children of the parent session;
  /// otherwise the parent agent's id (also a `SubAgentRow.id`).
  parentId: string | null;
  persona: string;
  status: SubAgentStatus;
  lastProgress: string | null;
}
