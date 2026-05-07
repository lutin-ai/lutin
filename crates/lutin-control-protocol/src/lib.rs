//! Control-panel tier payload definitions.
//!
//! Sits on top of `lutin-protocol::Frame`. The wire flow:
//! `Frame::Payload { body }` carries `postcard(Request | Response)`,
//! `Frame::Broadcast { body }` carries `postcard(Event)`.

pub use lutin_auth::{SessionId, Slug, WorkflowId};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use thiserror::Error;

/// Project display name. Non-empty, ≤ 128 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct DisplayName(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayNameError {
    Empty,
    TooLong,
}

impl fmt::Display for DisplayNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DisplayNameError::Empty => write!(f, "display name must not be empty"),
            DisplayNameError::TooLong => write!(f, "display name exceeds 128 chars"),
        }
    }
}

impl std::error::Error for DisplayNameError {}

impl DisplayName {
    pub fn parse(s: impl Into<String>) -> Result<Self, DisplayNameError> {
        let s = s.into();
        if s.is_empty() {
            return Err(DisplayNameError::Empty);
        }
        if s.len() > 128 {
            return Err(DisplayNameError::TooLong);
        }
        Ok(DisplayName(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DisplayName {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        DisplayName::parse(s).map_err(serde::de::Error::custom)
    }
}

/// ed25519 public key, exactly 32 bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProjectPubkey([u8; 32]);

impl ProjectPubkey {
    pub fn new(bytes: [u8; 32]) -> Self {
        ProjectPubkey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectInfo {
    pub slug: Slug,
    pub display_name: DisplayName,
}

/// Metadata about an installed workflow image, returned by
/// `ListWorkflows`. Sourced from the workflow image's labels — see
/// `lutin-control-panel/src/workflow_images.rs`. `digest` is the
/// underlying Docker image id; the desktop uses it as a cache key
/// for the bundle bytes fetched via `GetWorkflowBundle`.
///
/// `display_name` and `icon` come from `lutin.workflow.display_name`
/// / `lutin.workflow.icon` Docker labels and feed chrome's sidebar
/// + top-bar rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowInfo {
    pub id: WorkflowId,
    pub display_name: String,
    pub icon: String,
    pub digest: String,
}

/// One running or persisted session within a project. The session
/// itself is a separate WS endpoint the desktop dials directly — see
/// `SessionEndpoint`.
///
/// Sessions persist on disk independently of their containers: CP
/// indexes them in `<project>/.lutin/<workflow>/sessions.toml`, so a
/// stopped session is `Dormant` rather than gone. The chrome lists
/// dormant + running together; clicking dormant triggers
/// `ResumeSession` to bring the container back up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub workflow: WorkflowId,
    /// RFC3339 timestamp recorded when CP first started the session.
    /// Stable across stop/resume.
    pub created_at: String,
    /// `Running` if a container is currently up for this session id;
    /// `Dormant` otherwise. Computed at list-time from CP's registry —
    /// not stored on disk.
    pub state: SessionState,
    /// Workflow-supplied presentational metadata for the session list.
    /// Optional; chrome falls back to a generic label when absent.
    /// Engines write this into `<project>/.lutin/sessions/<id>/summary.json`
    /// while running and CP passes it through unchanged. The schema is
    /// the same for every workflow so chrome stays workflow-agnostic.
    pub summary: Option<SessionSummary>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionState {
    Running,
    Dormant,
}

/// Workflow-written, opaque-to-CP metadata that controls how a
/// session row is labelled in the desktop's list. Every field is
/// optional; chrome substitutes generic fallbacks when missing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    /// Headline for the row. Chat: first user message, truncated.
    /// Transcription: recording filename. Etc.
    pub title: Option<String>,
    /// Secondary line. Chat: "12 messages". Transcription: duration.
    pub subtitle: Option<String>,
    /// RFC3339 of the engine's last meaningful state change.
    pub last_activity: Option<String>,
    /// One-line preview body. Chat: last assistant message snippet.
    pub preview: Option<String>,
}

/// Where a started workflow session listens, and the token a client
/// should present when connecting directly to it. Token is signed by
/// the project keypair (CP holds it on behalf of each project) so the
/// session container can verify it offline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEndpoint {
    pub addr: std::net::SocketAddr,
    pub token: String,
    /// The project pubkey the session container will use to verify the
    /// `token` above. Returned alongside the endpoint so the desktop can
    /// pin it (it's per-project, stable across sessions).
    pub project_pubkey: ProjectPubkey,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Request {
    ListProjects,
    CreateProject {
        slug: Slug,
        display_name: DisplayName,
    },
    DeleteProject {
        slug: Slug,
    },
    /// Globally installed workflow images. Workflows are not yet
    /// per-project scoped; `slug` is reserved for forward-compat.
    ListWorkflows,
    /// Sessions known to CP for `slug` (running + persisted). CP is the
    /// authoritative source post-Phase-4 — there is no per-project
    /// supervisor maintaining this list.
    ListSessions {
        slug: Slug,
    },
    /// Spawn a new workflow-session container for `slug`, mint a
    /// session-scoped token signed by the project keypair, and return
    /// the bound addr + token via `ResponseOk::SessionStarted`.
    StartSession {
        slug: Slug,
        workflow: WorkflowId,
    },
    /// Stop a running session (terminates its container). The
    /// on-disk state and index entry are preserved — call
    /// `DeleteSession` to forget the session entirely.
    StopSession {
        slug: Slug,
        session: SessionId,
    },
    /// Bring a dormant session back up. Looks up the workflow id from
    /// the on-disk index, spawns a container against the existing
    /// session dir (so the engine reads its prior state), returns the
    /// new endpoint. No-op-with-fresh-token if the session is already
    /// running.
    ResumeSession {
        slug: Slug,
        session: SessionId,
    },
    /// Permanently remove a session: stop it if running, delete its
    /// state dir, drop the index entry. Irreversible.
    DeleteSession {
        slug: Slug,
        session: SessionId,
    },
    /// Re-issue a token + endpoint for an already-running session.
    /// Used when the desktop reconnects to a session it had open.
    OpenSession {
        slug: Slug,
        session: SessionId,
    },
    /// Fetch the static-asset bundle (tarball) for a workflow image.
    /// The bundle ships an HTML/JS plugin UI that runs in an iframe.
    /// Desktop caches by `(workflow_id, digest)` and only refetches
    /// when the digest reported by `ListWorkflows` moves.
    GetWorkflowBundle {
        id: WorkflowId,
    },
    /// Read the configured LLM providers from the global
    /// `settings.toml`. Only the providers list is exposed via this
    /// RPC today; other Settings sections (chat, tts, …) are still
    /// edited out-of-band.
    ListProviders,
    /// Replace the entire `providers = [...]` table in the global
    /// `settings.toml`. Whole-list replace is intentional: the
    /// desktop edits a draft and saves atomically, matching how it
    /// already handles connection profiles. Other sections of
    /// `settings.toml` are preserved by reading the file as
    /// `toml::Value`, swapping the providers key, and writing back.
    SetProviders {
        providers: Vec<ProviderConfig>,
    },
    /// Open a streaming transcription. Desktop calls this on PTT down,
    /// then pumps `TranscribeChunk` frames carrying mic samples while
    /// the key is held. Returns a `TranscriptionStreamId` that scopes
    /// subsequent chunk + finish + cancel calls.
    ///
    /// Whisper inference runs CP-side so the desktop doesn't need a
    /// model on disk and isn't bound by its memory ceiling. Config is
    /// per-stream rather than persisted CP-side: each desktop holds
    /// its own user prefs and just ships them along.
    OpenTranscription {
        config: WhisperConfig,
    },
    /// Append `samples` to the open stream. Wire format is
    /// `MonoPcm16k`: 16 kHz mono signed PCM, the only shape whisper
    /// accepts. The newtype carries that invariant from the cpal
    /// callback through to CP — neither side has to re-derive it
    /// from a doc-comment. Serialised transparently as `Vec<i16>` so
    /// the wire bytes are identical to a bare slice.
    ///
    /// Acked with `ChunkAccepted`. The desktop's pump task awaits
    /// each `cp_dispatch` before reading the next chunk from its
    /// audio receiver, so chunks are naturally serialised over the
    /// CP connection at one in flight per stream — that's the
    /// backpressure surface; the ack body itself carries no
    /// payload because the rate-limit signal is "did the call
    /// resolve."
    TranscribeChunk {
        stream_id: TranscriptionStreamId,
        samples: MonoPcm16k,
    },
    /// PTT released. CP runs whisper over the full accumulated buffer
    /// and replies with the decoded text. The stream is consumed —
    /// future chunk/finish calls against this id return
    /// `TranscriptionStreamNotFound`.
    FinishTranscription {
        stream_id: TranscriptionStreamId,
    },
    /// User cancelled mid-capture (e.g. tapped the key, immediate
    /// release, or chrome teardown). CP drops the buffer without
    /// running inference. Idempotent — a stream that's already gone
    /// returns `Cancelled` rather than erroring.
    CancelTranscription {
        stream_id: TranscriptionStreamId,
    },
    /// Pre-download / pre-load weights for a backend without opening a
    /// stream. Returns once the GGUF + SNAC (or backend-equivalent) are
    /// on disk and the factory has loaded into VRAM. Mirrors the
    /// whisper-model-fetch shape — workflows / settings UI call this
    /// from the user's "enable TTS" toggle so the first
    /// `OpenTtsStream` doesn't block for minutes on a fresh install.
    EnsureTtsBackend { backend: TtsBackend },
    /// Open a streaming TTS synthesis session against an
    /// already-loaded backend. Returns `TtsBackendNotReady` if the
    /// backend's weights haven't been loaded yet — call
    /// `EnsureTtsBackend` first.
    OpenTtsStream { backend: TtsBackend },
    /// Synthesise `text` on the open stream. Audio frames are pushed
    /// out-of-band as `Event::TtsAudio`, terminated by
    /// `Event::TtsFinished` (per `text` call). `speed` is in the
    /// 0.5..=2.0 range; the Orpheus backend currently ignores it
    /// (model has no speed control), but the wire carries it for
    /// future backends and post-output resampling.
    SpeakTts {
        stream_id: TtsStreamId,
        text: String,
        speed: TtsSpeed,
    },
    /// Drop in-flight synthesis + queued utterances on the stream.
    /// Idempotent — no-op once the stream is gone.
    CancelTts { stream_id: TtsStreamId },
    /// Tear down the stream. Subsequent `Speak`/`Cancel` against this
    /// id return `TtsStreamNotFound`.
    CloseTtsStream { stream_id: TtsStreamId },
}

/// Opaque handle for a streaming TTS session. CP allocates these
/// monotonically and passes the same value into `lutin_tts::StreamId`
/// (widened to `u64`) — single id space, no internal/external mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TtsStreamId(pub u32);

/// Which TTS backend to instantiate for a stream. Closed enum so the
/// wire surface can't pivot to arbitrary model files / hosts. Add new
/// variants for new backends; voice / per-utterance config rides
/// inside the variant that needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TtsBackend {
    Orpheus {
        model: OrpheusModel,
        voice: OrpheusVoice,
    },
}

/// Closed catalogue of Orpheus GGUF exports CP knows how to fetch.
/// Mirrors `WhisperModel` — wire surface stays tight, on-disk
/// filename + URL mapping lives CP-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrpheusModel {
    /// Maps to `orpheus-3b-0.1-ft-Q4_K_M.gguf` upstream. CP owns the
    /// variant → URL/filename mapping; the wire surface stays opaque.
    ThreeBQ4KM,
}

/// Playback speed for `SpeakTts`, expressed as integer thousandths
/// (`1000` ≡ 1.0×). Constrained to `[500, 2000]` (0.5×..=2.0×) at
/// parse time so the wire surface can't carry runaway values into a
/// future post-output resampler. The Orpheus backend currently
/// ignores speed (no in-model control); the value is carried for
/// future backends + cpal-stage resampling.
///
/// Integer-typed so the protocol's blanket `Eq` derive keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TtsSpeed(u16);

impl<'de> Deserialize<'de> for TtsSpeed {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = u16::deserialize(d)?;
        Self::from_thousandths(n).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtsSpeedOutOfRange {
    pub got_thousandths: u16,
}

impl fmt::Display for TtsSpeedOutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tts speed {}/1000 outside {}..={}",
            self.got_thousandths,
            TtsSpeed::MIN_THOUSANDTHS,
            TtsSpeed::MAX_THOUSANDTHS,
        )
    }
}

impl std::error::Error for TtsSpeedOutOfRange {}

impl TtsSpeed {
    pub const MIN_THOUSANDTHS: u16 = 500;
    pub const MAX_THOUSANDTHS: u16 = 2000;
    pub const NORMAL: Self = Self(1000);

    pub fn from_thousandths(n: u16) -> Result<Self, TtsSpeedOutOfRange> {
        if !(Self::MIN_THOUSANDTHS..=Self::MAX_THOUSANDTHS).contains(&n) {
            return Err(TtsSpeedOutOfRange { got_thousandths: n });
        }
        Ok(Self(n))
    }

    pub fn as_thousandths(self) -> u16 {
        self.0
    }

    pub fn as_f32(self) -> f32 {
        self.0 as f32 / 1000.0
    }
}

impl Default for TtsSpeed {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Documented voices for the Orpheus 3B 0.1-ft model. Closed enum so
/// a workflow can't smuggle arbitrary strings into the prompt
/// template. If a future model ships a different voice set, that
/// becomes a new `OrpheusVoice` variant or — when the sets diverge
/// enough — a new outer `TtsBackend` variant. The CP boundary maps
/// these to the backend-internal lowercase strings (`"tara"`,
/// `"leah"`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrpheusVoice {
    Tara,
    Leah,
    Jess,
    Leo,
    Dan,
    Mia,
    Zac,
    Zoe,
}

/// Opaque handle for a streaming transcription. CP allocates these
/// monotonically; the desktop just echoes the value back. `u32` is
/// plenty — at most a handful of streams live at once and the counter
/// resets every CP boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TranscriptionStreamId(pub u32);

/// 16 kHz mono signed PCM — the only audio shape whisper accepts.
///
/// Constructed once at the cpal capture boundary, after resampling
/// + downmixing; carried through the desktop's chunk pump and the
/// CP-side stream buffer without any further validation. Storing the
/// invariant in the type means a downstream `&MonoPcm16k` is *proof*
/// of the format rather than a doc-comment promise.
///
/// Wire format is the inner `Vec<i16>` exactly — `#[serde(transparent)]`
/// — so this newtype is free at the protocol layer and trivially
/// removable if the contract ever loosens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MonoPcm16k(Vec<i16>);

impl MonoPcm16k {
    /// Take ownership of `samples` already known to be 16 kHz mono
    /// PCM. The capture pipeline calls this exactly once per chunk;
    /// downstream code just borrows.
    pub fn from_samples(samples: Vec<i16>) -> Self {
        Self(samples)
    }

    pub fn as_slice(&self) -> &[i16] {
        &self.0
    }

    pub fn into_inner(self) -> Vec<i16> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Closed catalogue of whisper.cpp models CP knows how to download.
/// Mirrors the on-disk filename and Hugging Face URL so a malicious
/// JSON payload can't pivot to arbitrary files or hosts. Add new
/// entries here, never accept free-form filenames over the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhisperModel {
    LargeV3Turbo,
    DistilLargeV3,
}

impl Default for WhisperModel {
    fn default() -> Self {
        Self::LargeV3Turbo
    }
}

/// Sampling strategy for one transcription. `Greedy` is fastest;
/// `Beam(n)` trades CPU for accuracy. Persisted as a plain integer so
/// the JSON config stays human-editable: `0` and `1` round-trip to
/// `Greedy`, values >=2 become `Beam(n)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeamSize {
    Greedy,
    Beam(std::num::NonZeroU8),
}

impl Default for BeamSize {
    fn default() -> Self {
        Self::Beam(std::num::NonZeroU8::new(5).expect("5 != 0"))
    }
}

impl Serialize for BeamSize {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Greedy => 1u8.serialize(s),
            Self::Beam(n) => n.get().serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for BeamSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = u8::deserialize(d)?;
        Ok(match n {
            0 | 1 => Self::Greedy,
            n => Self::Beam(std::num::NonZeroU8::new(n).expect("n > 1")),
        })
    }
}

/// Per-stream transcription parameters. The desktop holds the user
/// prefs and ships them with `OpenTranscription` so CP stays
/// stateless across stream lifetimes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WhisperConfig {
    pub model: WhisperModel,
    /// Whisper language code ("en", "sv", …) or `None` for autodetect.
    pub language: Option<String>,
    pub beam_size: BeamSize,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model: WhisperModel::default(),
            language: None,
            beam_size: BeamSize::default(),
        }
    }
}

/// Plain DTO mirroring `lutin_settings::ProviderConfig`. Defined here
/// (rather than re-exported) to keep `lutin-control-protocol` dep-light;
/// the CP runtime converts between the two when reading/writing
/// `settings.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub use_oauth: bool,
}

/// Wire/disk format uses snake_case (`open_router`, `open_ai_compat`)
/// to match `lutin_settings::ProviderKind` so the on-disk
/// `settings.toml` written by the CP handler stays compatible with
/// the engine-side loader.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenRouter,
    Ollama,
    Anthropic,
    OpenAiCompat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Response {
    Ok(ResponseOk),
    Err(ApiError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResponseOk {
    Projects(Vec<ProjectInfo>),
    Created(ProjectInfo),
    Deleted,
    Workflows(Vec<WorkflowInfo>),
    Sessions(Vec<SessionInfo>),
    /// Reply to `StartSession` — carries the new session metadata plus
    /// its WS endpoint so the desktop can dial in the same round-trip.
    SessionStarted {
        info: SessionInfo,
        endpoint: SessionEndpoint,
    },
    SessionStopped,
    SessionDeleted,
    /// Reply to `ResumeSession`: a fresh endpoint plus the rehydrated
    /// `SessionInfo` (state will be `Running` after this returns).
    SessionResumed {
        info: SessionInfo,
        endpoint: SessionEndpoint,
    },
    /// Reply to `OpenSession`: just an endpoint (the caller already has
    /// the `SessionInfo` from `ListSessions`).
    SessionOpened(SessionEndpoint),
    /// Reply to `GetWorkflowBundle`. `bytes` is a tar archive of the
    /// plugin UI (root-level `lutin.workflow.json` + `index.html` + any
    /// referenced assets). Desktop unpacks under its cache dir keyed
    /// by `(workflow_id, digest)`.
    WorkflowBundle {
        id: WorkflowId,
        digest: String,
        bytes: Vec<u8>,
    },
    /// Reply to `ListProviders`.
    Providers(Vec<ProviderConfig>),
    /// Reply to `SetProviders`.
    ProvidersSaved,
    /// Reply to `OpenTranscription`.
    TranscriptionOpened {
        stream_id: TranscriptionStreamId,
    },
    /// Reply to `TranscribeChunk`. Carries no payload — the desktop
    /// uses these as ack pacing for the next chunk.
    ChunkAccepted,
    /// Reply to `FinishTranscription`. `text` is the decoded output;
    /// empty string when whisper produced nothing usable (silence,
    /// sub-threshold clip).
    Transcription {
        text: String,
    },
    /// Reply to `CancelTranscription`. Idempotent — fires for both
    /// "stream existed and was cancelled" and "stream id was already
    /// gone".
    TranscriptionCancelled,
    /// Reply to `EnsureTtsBackend`. Carries no payload — backend is
    /// either loaded or `TtsBackendUnavailable` came back instead.
    TtsBackendReady,
    /// Reply to `OpenTtsStream`.
    TtsStreamOpened { stream_id: TtsStreamId },
    /// Reply to `SpeakTts`. Carries no payload — synthesis output is
    /// pushed via `Event::TtsAudio` / `Event::TtsFinished`.
    TtsSpeechQueued,
    /// Reply to `CancelTts`. Idempotent — fires for both
    /// "stream existed and was cancelled" and "stream id was already
    /// gone".
    TtsCancelled,
    /// Reply to `CloseTtsStream`.
    TtsStreamClosed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
pub enum ApiError {
    #[error("project not found: {0}")]
    NotFound(Slug),
    #[error("project already exists: {0}")]
    AlreadyExists(Slug),
    #[error("supervisor: {0}")]
    Supervisor(String),
    #[error("workflow not found: {0}")]
    WorkflowNotFound(WorkflowId),
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("settings: {0}")]
    Settings(String),
    #[error("transcription stream not found: {0:?}")]
    TranscriptionStreamNotFound(TranscriptionStreamId),
    /// Wire-bound limits: chunk too large, or per-connection stream
    /// quota exhausted. Carries the offending value so a misbehaving
    /// client can self-diagnose.
    #[error("transcription limit exceeded: {0}")]
    TranscriptionLimit(TranscriptionLimit),
    /// Whisper model file is not available locally and a download
    /// was attempted but failed (network, disk, validation). Distinct
    /// from `Inference` so the desktop can surface a different
    /// message and consider retrying.
    #[error("whisper model unavailable: {0}")]
    WhisperModelUnavailable(String),
    /// Whisper inference itself failed (decode error, context
    /// rejected the audio, internal panic). Generic catch-all for the
    /// last leg of the pipeline.
    #[error("whisper inference: {0}")]
    WhisperInference(String),
    #[error("tts stream not found: {0:?}")]
    TtsStreamNotFound(TtsStreamId),
    /// `OpenTtsStream` arrived before `EnsureTtsBackend` for the
    /// matching backend identity. Desktop should call
    /// `EnsureTtsBackend` (showing progress) and retry the open.
    #[error("tts backend not ready")]
    TtsBackendNotReady,
    /// Backend weights couldn't be made available — download failed,
    /// disk full, factory load errored. Distinct from
    /// `TtsBackendNotReady` (a sequencing error) so the desktop can
    /// surface a real error message.
    #[error("tts backend unavailable: {0}")]
    TtsBackendUnavailable(String),
    /// TTS synthesis failed mid-utterance (model error, GPU OOM, …).
    #[error("tts synthesis: {0}")]
    TtsSynthesis(String),
    /// Wire-bound limits: too many open streams, text too long.
    #[error("tts limit exceeded: {0}")]
    TtsLimit(TtsLimit),
}

/// Specific limit a TTS request blew through. Same shape as
/// `TranscriptionLimit` — small closed enum so the desktop can switch
/// on the cause without parsing a string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TtsLimit {
    /// Process-wide open-stream count would exceed
    /// `MAX_OPEN_STREAMS`. Almost always a workflow bug
    /// (failure to `CloseTtsStream`).
    TooManyStreams { max: usize },
    /// `text.len()` (in bytes) exceeded the per-`SpeakTts` cap. The
    /// model has a fixed context window, so longer inputs are
    /// silently truncated by the worker; rejecting at the boundary
    /// means the workflow gets a hard error rather than mysterious
    /// half-spoken sentences.
    TextTooLong { got: usize, max: usize },
}

impl fmt::Display for TtsLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyStreams { max } => {
                write!(f, "too many open tts streams (max {max})")
            }
            Self::TextTooLong { got, max } => {
                write!(f, "tts text too long: {got} bytes > max {max}")
            }
        }
    }
}

/// Specific limit a `TranscribeChunk` / `OpenTranscription` request
/// blew through. Kept as a small enum (rather than a number + label)
/// so the desktop can switch on the cause without parsing a string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TranscriptionLimit {
    /// `samples.len()` exceeded `MAX_CHUNK_SAMPLES`. The protocol
    /// doesn't fix the constant — CP picks it; the desktop just
    /// reads the cap from the error.
    ChunkTooLarge { got: usize, max: usize },
    /// Per-connection open-stream count would exceed
    /// `MAX_OPEN_STREAMS_PER_CONN`. Almost always a desktop bug
    /// (failure to `Cancel`/`Finish`); user-visible only as a
    /// "transcription rejected" toast.
    TooManyStreams { max: usize },
}

impl fmt::Display for TranscriptionLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChunkTooLarge { got, max } => {
                write!(f, "chunk too large: {got} samples > max {max}")
            }
            Self::TooManyStreams { max } => {
                write!(f, "too many open streams (max {max} per connection)")
            }
        }
    }
}

/// Server-pushed events, fanned out to every authenticated client.
/// Session events carry `slug` so a single CP WS conn carries traffic
/// for every project the client cares about; the client filters by slug.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    ProjectCreated(ProjectInfo),
    ProjectDeleted { slug: Slug },
    SessionStarted { slug: Slug, info: SessionInfo },
    SessionEnded { slug: Slug, session: SessionId },
    /// Synthesised audio frame for an open TTS stream. Broadcast —
    /// every authenticated client receives it; clients filter by the
    /// `stream_id` they own. `chunk` is raw PCM (24 kHz mono i16
    /// little-endian for the Orpheus backend).
    TtsAudio {
        stream_id: TtsStreamId,
        chunk: Vec<u8>,
    },
    /// Terminator for a single `SpeakTts` call. Pairs 1:1 with
    /// `SpeakTts`, *not* with the stream lifetime — the same stream
    /// emits one `Finished` per utterance.
    TtsFinished { stream_id: TtsStreamId },
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    Ok(postcard::to_allocvec(value)?)
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    Ok(postcard::from_bytes(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = Request::CreateProject {
            slug: Slug::parse("foo").unwrap(),
            display_name: DisplayName::parse("Foo").unwrap(),
        };
        assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn response_roundtrip() {
        let r = Response::Ok(ResponseOk::Created(ProjectInfo {
            slug: Slug::parse("foo").unwrap(),
            display_name: DisplayName::parse("Foo").unwrap(),
        }));
        assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn event_roundtrip() {
        let e = Event::ProjectDeleted {
            slug: Slug::parse("foo").unwrap(),
        };
        assert_eq!(decode::<Event>(&encode(&e).unwrap()).unwrap(), e);
    }

    #[test]
    fn err_response_roundtrip() {
        let r = Response::Err(ApiError::NotFound(Slug::parse("x").unwrap()));
        assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn open_transcription_roundtrip() {
        let r = Request::OpenTranscription {
            config: WhisperConfig {
                model: WhisperModel::DistilLargeV3,
                language: Some("en".into()),
                beam_size: BeamSize::Beam(std::num::NonZeroU8::new(3).unwrap()),
            },
        };
        assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn transcribe_chunk_roundtrip() {
        let r = Request::TranscribeChunk {
            stream_id: TranscriptionStreamId(7),
            samples: MonoPcm16k::from_samples(vec![0, 1, -1, i16::MAX, i16::MIN]),
        };
        assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn finish_and_cancel_transcription_roundtrip() {
        let f = Request::FinishTranscription {
            stream_id: TranscriptionStreamId(42),
        };
        assert_eq!(decode::<Request>(&encode(&f).unwrap()).unwrap(), f);
        let c = Request::CancelTranscription {
            stream_id: TranscriptionStreamId(42),
        };
        assert_eq!(decode::<Request>(&encode(&c).unwrap()).unwrap(), c);
    }

    #[test]
    fn transcription_response_variants_roundtrip() {
        for r in [
            Response::Ok(ResponseOk::TranscriptionOpened {
                stream_id: TranscriptionStreamId(1),
            }),
            Response::Ok(ResponseOk::ChunkAccepted),
            Response::Ok(ResponseOk::Transcription {
                text: "hello world".into(),
            }),
            Response::Ok(ResponseOk::TranscriptionCancelled),
        ] {
            assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
        }
    }

    #[test]
    fn transcription_error_variants_roundtrip() {
        for r in [
            Response::Err(ApiError::TranscriptionStreamNotFound(
                TranscriptionStreamId(99),
            )),
            Response::Err(ApiError::TranscriptionLimit(
                TranscriptionLimit::ChunkTooLarge {
                    got: 200_000,
                    max: 160_000,
                },
            )),
            Response::Err(ApiError::TranscriptionLimit(
                TranscriptionLimit::TooManyStreams { max: 32 },
            )),
            Response::Err(ApiError::WhisperModelUnavailable("404 from hf".into())),
            Response::Err(ApiError::WhisperInference("decode failed".into())),
        ] {
            assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
        }
    }

    /// `BeamSize` has hand-rolled serde mapping `0|1 → Greedy`,
    /// `≥2 → Beam(n)`. Easy to break by reordering the match arms;
    /// pin every relevant value.
    #[test]
    fn beam_size_serde() {
        // Greedy serialises as `1`.
        let g_bytes = encode(&BeamSize::Greedy).unwrap();
        assert_eq!(g_bytes, encode(&1u8).unwrap());
        assert_eq!(decode::<BeamSize>(&g_bytes).unwrap(), BeamSize::Greedy);

        // `0` deserialises back to Greedy.
        let zero = encode(&0u8).unwrap();
        assert_eq!(decode::<BeamSize>(&zero).unwrap(), BeamSize::Greedy);

        // Beam(5) round-trips.
        let b5 = BeamSize::Beam(std::num::NonZeroU8::new(5).unwrap());
        assert_eq!(decode::<BeamSize>(&encode(&b5).unwrap()).unwrap(), b5);
    }

    #[test]
    fn tts_speed_parse_and_serde() {
        assert_eq!(
            TtsSpeed::from_thousandths(1000).unwrap(),
            TtsSpeed::NORMAL
        );
        assert_eq!(TtsSpeed::from_thousandths(500).unwrap().as_thousandths(), 500);
        assert_eq!(TtsSpeed::from_thousandths(2000).unwrap().as_thousandths(), 2000);
        assert!(TtsSpeed::from_thousandths(499).is_err());
        assert!(TtsSpeed::from_thousandths(2001).is_err());

        // Wire layer enforces the same range — a hand-rolled u16 that
        // bypasses `from_thousandths` must not deserialise.
        let bad = encode(&100u16).unwrap();
        assert!(decode::<TtsSpeed>(&bad).is_err());

        // Round-trip a valid value.
        let s = TtsSpeed::from_thousandths(1250).unwrap();
        assert_eq!(decode::<TtsSpeed>(&encode(&s).unwrap()).unwrap(), s);
    }

    #[test]
    fn open_tts_stream_roundtrip() {
        let r = Request::OpenTtsStream {
            backend: TtsBackend::Orpheus {
                model: OrpheusModel::ThreeBQ4KM,
                voice: OrpheusVoice::Tara,
            },
        };
        assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn tts_request_variants_roundtrip() {
        for r in [
            Request::EnsureTtsBackend {
                backend: TtsBackend::Orpheus {
                    model: OrpheusModel::ThreeBQ4KM,
                    voice: OrpheusVoice::Leah,
                },
            },
            Request::SpeakTts {
                stream_id: TtsStreamId(7),
                text: "hello world".into(),
                speed: TtsSpeed::NORMAL,
            },
            Request::CancelTts {
                stream_id: TtsStreamId(7),
            },
            Request::CloseTtsStream {
                stream_id: TtsStreamId(7),
            },
        ] {
            assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
        }
    }

    #[test]
    fn tts_response_variants_roundtrip() {
        for r in [
            Response::Ok(ResponseOk::TtsBackendReady),
            Response::Ok(ResponseOk::TtsStreamOpened {
                stream_id: TtsStreamId(3),
            }),
            Response::Ok(ResponseOk::TtsSpeechQueued),
            Response::Ok(ResponseOk::TtsCancelled),
            Response::Ok(ResponseOk::TtsStreamClosed),
        ] {
            assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
        }
    }

    #[test]
    fn tts_error_variants_roundtrip() {
        for r in [
            Response::Err(ApiError::TtsStreamNotFound(TtsStreamId(99))),
            Response::Err(ApiError::TtsBackendNotReady),
            Response::Err(ApiError::TtsBackendUnavailable("404 from hf".into())),
            Response::Err(ApiError::TtsSynthesis("decode failed".into())),
            Response::Err(ApiError::TtsLimit(TtsLimit::TooManyStreams { max: 32 })),
            Response::Err(ApiError::TtsLimit(TtsLimit::TextTooLong {
                got: 9000,
                max: 4096,
            })),
        ] {
            assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
        }
    }

    #[test]
    fn tts_event_variants_roundtrip() {
        for e in [
            Event::TtsAudio {
                stream_id: TtsStreamId(1),
                chunk: vec![1, 2, 3, 4],
            },
            Event::TtsFinished {
                stream_id: TtsStreamId(1),
            },
        ] {
            assert_eq!(decode::<Event>(&encode(&e).unwrap()).unwrap(), e);
        }
    }

    /// `MonoPcm16k` is `#[serde(transparent)]` — confirm the wire
    /// bytes equal those of a bare `Vec<i16>`. Catches accidental
    /// addition of a wrapper variant or rename.
    #[test]
    fn mono_pcm_transparent() {
        let raw: Vec<i16> = vec![10, 20, 30, -40];
        let wrapped = MonoPcm16k::from_samples(raw.clone());
        assert_eq!(encode(&wrapped).unwrap(), encode(&raw).unwrap());
    }
}
