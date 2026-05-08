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
