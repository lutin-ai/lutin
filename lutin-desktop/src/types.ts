// Mirrors lutin-control-protocol Rust enums via serde's default JSON
// representation (externally-tagged: unit variants serialize as bare
// strings, struct variants as `{ Variant: { fields } }`).

export type Slug = string;
export type SessionId = string;
export type WorkflowId = string;
export type DisplayName = string;

export interface ProjectInfo {
  slug: Slug;
  display_name: DisplayName;
}

export interface WorkflowInfo {
  id: WorkflowId;
  display_name: string;
  icon: string;
  digest: string;
}

export type SessionState = "Running" | "Dormant";

/// Workflow-written, opaque-to-CP metadata that controls how a
/// session row is labelled. Every field optional — chrome falls back
/// to a generic label when missing.
export interface SessionSummary {
  title?: string | null;
  subtitle?: string | null;
  last_activity?: string | null;
  preview?: string | null;
  persona?: string | null;
  model?: string | null;
  total_prompt_tokens?: number | null;
  total_completion_tokens?: number | null;
  context_tokens?: number | null;
  message_count?: number | null;
}

export interface SessionInfo {
  id: SessionId;
  workflow: WorkflowId;
  /// RFC3339, recorded when CP first started the session. Stable
  /// across stop/resume.
  created_at: string;
  state: SessionState;
  summary?: SessionSummary | null;
}

export interface SessionEndpoint {
  addr: string;
  token: string;
  project_pubkey: number[];
}

export type ProviderKind =
  | "open_router"
  | "ollama"
  | "anthropic"
  | "open_ai_compat";

export interface WebSearchSettings {
  brave_api_key?: string | null;
}

export interface ProviderConfig {
  name: string;
  kind: ProviderKind;
  api_key?: string | null;
  api_key_env?: string | null;
  base_url?: string | null;
  use_oauth?: boolean;
}

export type Request =
  | "ListProjects"
  | { CreateProject: { slug: Slug; display_name: DisplayName } }
  | { DeleteProject: { slug: Slug } }
  | "ListWorkflows"
  | { ListSessions: { slug: Slug } }
  | { StartSession: { slug: Slug; workflow: WorkflowId } }
  | { StopSession: { slug: Slug; session: SessionId } }
  | { ResumeSession: { slug: Slug; session: SessionId } }
  | { DeleteSession: { slug: Slug; session: SessionId } }
  | { OpenSession: { slug: Slug; session: SessionId } }
  | { GetWorkflowBundle: { id: WorkflowId } }
  | "ListProviders"
  | { SetProviders: { providers: ProviderConfig[] } }
  | "GetWebSearch"
  | { SetWebSearch: { settings: WebSearchSettings } };

export type ResponseOk =
  | { Projects: ProjectInfo[] }
  | { Created: ProjectInfo }
  | "Deleted"
  | { Workflows: WorkflowInfo[] }
  | { Sessions: SessionInfo[] }
  | { SessionStarted: { info: SessionInfo; endpoint: SessionEndpoint } }
  | "SessionStopped"
  | "SessionDeleted"
  | { SessionResumed: { info: SessionInfo; endpoint: SessionEndpoint } }
  | { SessionOpened: SessionEndpoint }
  | { WorkflowBundle: { id: WorkflowId; digest: string; bytes: number[] } }
  | { Providers: ProviderConfig[] }
  | "ProvidersSaved"
  | { WebSearch: WebSearchSettings }
  | "WebSearchSaved";

export type ApiError =
  | { NotFound: Slug }
  | { AlreadyExists: Slug }
  | { Supervisor: string }
  | { WorkflowNotFound: WorkflowId }
  | { SessionNotFound: SessionId }
  | { Settings: string };

export type Response = { Ok: ResponseOk } | { Err: ApiError };

export type CpEvent =
  | { ProjectCreated: ProjectInfo }
  | { ProjectDeleted: { slug: Slug } }
  | { SessionStarted: { slug: Slug; info: SessionInfo } }
  | { SessionEnded: { slug: Slug; session: SessionId } }
  // `TtsAudio` is intercepted in Rust (`drain_updates`) and routed
  // to the playback module — never reaches JS — so it's intentionally
  // not in this union. `TranscriptionPartial` is similarly intercepted
  // and folded into the overlay phase polled via `overlay_current_phase`.
  // The remaining TTS variants do.
  | { TtsFinished: { stream_id: number } }
  | {
      TtsBackendDownload: {
        backend: TtsBackend;
        file: string;
        downloaded: number;
        total: number | null;
      };
    };

export interface ConnectionProfile {
  name: string;
  addr: string;
  token: string;
}

/// Mirrors Rust `Action` (settings.rs). Externally tagged on `kind` —
/// only `ptt` exists today; reserved variants (`dictate`, `open_mic_toggle`,
/// …) are additive so old settings round-trip through future builds.
export type Action = { kind: "ptt" };

/// Mirrors Rust `Target` (settings.rs). `active_workflow` resolves at
/// dispatch time against the focused iframe; `workflow` pins to a
/// specific workflow id; `clipboard` is the safe fallback.
export type Target =
  | { kind: "active_workflow" }
  | { kind: "workflow"; workflow: WorkflowId }
  | { kind: "clipboard" };

export interface KeyBind {
  combo: string;
  action: Action;
  target: Target;
}

export type WhisperModel = "large-v3-turbo" | "distil-large-v3";

// Wire-shape TTS types are shared with the workflow side via
// `@lutin/shim-types`; re-exported here so `api.ts` / `PluginIframe`
// callers don't have to reach into the shared package directly.
import type { TtsBackend } from "@lutin/shim-types";
export type {
  OrpheusModel,
  OrpheusVoice,
  SubAgentRow,
  SubAgentStatus,
  TtsBackend,
  TtsStreamId,
} from "@lutin/shim-types";

// `TtsSpeed` is integer thousandths in `[500, 2000]`. The Rust
// `Deserialize` impl rejects out-of-range values at the Tauri serde
// boundary, so a bad number surfaces as an invoke error. `1000` ≡
// 1.0×; treat this as opaque on the JS side and let helpers build it.
// Workflow side never sees this form — the shim takes a `1.0×`-style
// multiplier and converts — so it doesn't live in `@lutin/shim-types`.
export type TtsSpeed = number;

/// Mirrors Rust `WhisperConfig`. `beam_size` is persisted as a small
/// integer (0/1 ⇒ greedy, n ⇒ beam width n); we keep that shape so
/// edits round-trip cleanly through the chrome's draft state.
export interface WhisperConfig {
  model: WhisperModel;
  language?: string | null;
  beam_size: number;
}

export type ParakeetModel = "tdt06b-v3";

/// Mirrors Rust `ParakeetConfig`. Multilingual auto-detect — no
/// language hint or beam knob today, just the model id.
export interface ParakeetConfig {
  model: ParakeetModel;
}

/// Mirrors Rust `SttConfig` — externally tagged on the variant name
/// (same shape as `TtsBackend`). The desktop only constructs one of
/// these in `dispatch.rs` from settings + ships it to CP.
export type SttConfig =
  | { Whisper: WhisperConfig }
  | { Parakeet: ParakeetConfig };

/// Mirrors Rust `AudioSettings` (settings.rs). `null` ⇒ host default;
/// values are cpal device names (`Device::description().name()`).
export interface AudioSettings {
  input: string | null;
  output: string | null;
}

export interface DesktopSettings {
  default: string;
  connections: ConnectionProfile[];
  keybinds: KeyBind[];
  stt: SttConfig;
  audio: AudioSettings;
}

// Mirrors Rust `PluginManifest` + `PluginOpened` (lib.rs). `url` is
// the iframe `src`; the React side never constructs it, since the
// custom-protocol URL form differs by platform.
export interface PluginManifest {
  entry: string;
  permissions: string[];
  /// Subset of well-known capability names this workflow can be the
  /// *target* of (vs `permissions`, which is what it can *do*). E.g.
  /// `"receive_transcription"` opts in to per-session transcription
  /// deliveries from the chrome's hotkey routing.
  capabilities: string[];
  display_name: string;
  icon: string;
}

/// Sent to Rust via `set_active_session` whenever a plugin iframe
/// mounts or unmounts. Mirrors Rust `ActiveSession`. Carrying
/// `capabilities` here avoids an extra round-trip through the bundle
/// cache on the dispatch hot path.
export interface ActiveSession {
  session: SessionId;
  workflow: WorkflowId;
  capabilities: string[];
}

export interface PluginOpened {
  url: string;
  manifest: PluginManifest;
}

// Mirrors Rust `ConnSnapshot` (lib.rs) — externally tagged on `kind`,
// lowercase variants. The App store's connection state has the same
// shape, so a `cp_status` invoke can hydrate it directly.
export type ConnState =
  | { kind: "noconfig" }
  | { kind: "connecting" }
  | { kind: "connected" }
  | { kind: "disconnected" }
  | { kind: "rejected"; reason: string }
  | { kind: "error"; error: string };
