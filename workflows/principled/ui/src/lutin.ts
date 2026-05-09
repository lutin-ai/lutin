// Plugin-side bindings for the chrome-hosted shim. The shim itself
// lives in the desktop crate and is served from
// `lutin-shim://localhost/shim.js` — see `index.html`. By the time
// any bundled module runs, the shim has already attached
// `window.__lutinReady`, a Promise that resolves with the Lutin API
// once chrome's `lutin-init` postMessage lands.
//
// Plugins should never import a runtime shim from this file; we only
// re-export the types and a thin awaiter for the global so callers
// can stay strictly typed.

export interface PluginManifest {
  entry: string;
  permissions: string[];
  /// Capabilities this workflow opts into receiving from the chrome.
  /// Slice 3 uses `"receive_transcription"`.
  capabilities: string[];
  display_name: string;
  icon: string;
}

export interface TranscriptionMessage {
  text: string;
  source: "ptt" | "openmic";
}

// TTS shim surface — only present when the workflow declares `"tts"`
// in `capabilities`. Mirrors the chrome-side wrappers in
// `lutin-desktop/src/api.ts`. `TtsStreamId` is opaque to JS — pass
// what `openStream` returns straight through to the other methods.
// Wire-shape types are shared with chrome via `@lutin/shim-types`.
export type {
  OrpheusModel,
  OrpheusVoice,
  SubAgentRow,
  SubAgentStatus,
  TtsBackend,
  TtsStreamId,
} from "@lutin/shim-types";
import type { SubAgentRow, TtsBackend, TtsStreamId } from "@lutin/shim-types";

export interface LutinTts {
  ensureBackend(backend: TtsBackend): Promise<void>;
  openStream(backend: TtsBackend): Promise<TtsStreamId>;
  /// `opts.speed` is a multiplier (`1.0` = normal); the shim converts
  /// to the wire's integer-thousandths form. The chrome will reject
  /// values outside `0.5..=2.0`.
  speak(streamId: TtsStreamId, text: string, opts?: { speed?: number }): Promise<void>;
  cancel(streamId: TtsStreamId): Promise<void>;
  closeStream(streamId: TtsStreamId): Promise<void>;
}

export interface LutinInit {
  slug: string;
  session: string;
  workflow: string;
  manifest: PluginManifest;
}

export interface Lutin extends LutinInit {
  request(body: Uint8Array): Promise<Uint8Array>;
  onBroadcast(cb: (body: Uint8Array) => void): () => void;
  notify(body: string, title?: string): void;
  /// Only present when the workflow's manifest declares
  /// `receive_transcription` in `capabilities`. Subscribe to receive
  /// PTT / open-mic transcription deliveries routed by chrome.
  onTranscription?(cb: (msg: TranscriptionMessage) => void): () => void;
  /// Only present when the workflow's manifest declares `"tts"` in
  /// `capabilities`. Audio playback is fully chrome-side; workflows
  /// just feed text and react to `closeStream` resolutions.
  tts?: LutinTts;
  /// Only present when the workflow's manifest declares
  /// `"sub_agents"` in `capabilities`. Push the current sub-agent
  /// registry to the chrome — the desktop renders it as indented
  /// rows under the parent chat. Send the full list each time;
  /// the sidebar replaces rather than diffs.
  publishSubAgents?(agents: SubAgentRow[]): void;
  /// Paired with `publishSubAgents`; gated by the same capability.
  /// Subscribe to chrome-driven selection events. Selection is
  /// UI-only — the chrome decides what's focused, the iframe just
  /// reflects the choice. `null` means "show the parent chat".
  onSelectSubAgent?(cb: (id: string | null) => void): () => void;
  /// Push the running session summary up to the chrome. Always
  /// available — the sidebar wants ctx + lifetime totals for every
  /// workflow, so this isn't capability-gated. Callers invoke this
  /// once per `summaryUpdated` broadcast; sessions that have not yet
  /// produced a usage report simply don't call it, and the sidebar
  /// shows em-dash placeholders until the first push lands.
  publishSummary(summary: SessionSummaryUpdate): void;
}

/** Live counters the iframe pushes to chrome on every
 *  `summaryUpdated` broadcast. Mirrors the Rust `ChatEvent::SummaryUpdated`
 *  payload but flattens to JSON-friendly numbers. */
export interface SessionSummaryUpdate {
  contextTokens: number | null;
  totalPromptTokens: number;
  totalCompletionTokens: number;
  /** Active persona name, or `null` if the session has none selected.
   *  Pushed alongside the token tick so the chrome's sidebar reflects
   *  persona switches without waiting for a `ListSessions` poll. */
  persona?: string | null;
  /** Auto-derived session title (typically the first user message,
   *  truncated). Same motivation as `persona`: keep the sidebar label
   *  live without polling `summary.json`. */
  title?: string | null;
}

declare global {
  interface Window {
    __lutinReady?: Promise<Lutin>;
    lutin?: Lutin;
  }
}

export function getLutin(): Promise<Lutin> {
  const ready = window.__lutinReady;
  if (!ready) {
    return Promise.reject(
      new Error(
        "lutin shim missing — chrome did not serve lutin-shim://localhost/shim.js",
      ),
    );
  }
  return ready;
}
