//! Chat workflow protocol + per-session state.
//!
//! The chat workflow runs as its own subprocess (one per session)
//! spawned by CP. It does not share `lutin-session-protocol` with the
//! project tier — workflows define their own request/response shapes.
//! The wire envelope is `lutin_protocol::Frame`; payloads ride inside
//! `Frame::Payload.body` / `Frame::Broadcast.body` as postcard-encoded
//! values of the types declared here. Protocol items live at the crate
//! root so `engine.rs` can keep its existing `use principled::{ChatRequest, …}`
//! imports. The plugin UI lives in `ui/` (a static asset bundle shipped
//! in the Docker image), not in this crate.

use std::path::{Path, PathBuf};

use lutin_workflow_sdk::state as sdk_state;
use serde::de::{DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Treat `Some("")` as `None`. Pushes the "empty-string-as-absent"
/// invariant to the boundary so downstream code can rely on
/// `Option::Some(_)` carrying a non-empty string.
fn deserialize_non_empty_opt<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    Ok(raw.filter(|s| !s.is_empty()))
}

/// Monotonically increasing identifier for one user-message → assistant
/// completion turn. Allocated by the engine on `SendMessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(pub u64);

/// Why the assistant stopped producing output for a turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishReason {
    Completed,
    Cancelled,
    Failed(String),
    /// The SDK hit its `max_rounds` cap before the agent decided to
    /// stop. Surfaced as a distinct variant (rather than `Failed`) so
    /// the UI can show it as an interrupted-but-not-broken outcome —
    /// the user can simply ask the agent to continue.
    MaxRounds,
}

/// Persistent per-session settings. Lives at
/// `<state_dir>/state.toml` and is reloaded on every user message so
/// out-of-band edits take effect without restarting the workflow.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionState {
    /// Persona name (file stem in `personas/`). `None` means use the
    /// engine-side default.
    #[serde(default, deserialize_with = "deserialize_non_empty_opt")]
    pub persona: Option<String>,
    /// Optional model override; takes precedence over the persona's
    /// configured model when set.
    #[serde(default, deserialize_with = "deserialize_non_empty_opt")]
    pub model_override: Option<String>,
    /// Maximum number of reviewer LLM calls to run concurrently per
    /// review frame. `None` falls back to [`DEFAULT_REVIEW_CONCURRENCY`].
    /// Bounding this keeps a single shared LLM backend from being
    /// overwhelmed by a fan-out across many principles — every
    /// reviewer call is a streaming SSE request against the same
    /// vLLM instance, and oversubscription has caused upstream
    /// deadlocks.
    #[serde(default)]
    pub review_concurrency: Option<usize>,
}

/// Default reviewer concurrency. Sized for a single-GPU vLLM backend
/// whose KV cache caps `Running` around 12; 8 leaves headroom for the
/// agent's own round-trip and concurrent sessions.
pub const DEFAULT_REVIEW_CONCURRENCY: usize = 8;

impl SessionState {
    /// Resolve `review_concurrency`, applying the default and clamping
    /// 0 (which would deadlock the fan-out) up to 1.
    pub fn review_concurrency(&self) -> usize {
        self.review_concurrency
            .map(|n| n.max(1))
            .unwrap_or(DEFAULT_REVIEW_CONCURRENCY)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatRequest {
    /// Subscribe to live `ChatEvent` broadcasts and receive the current
    /// `SessionState` in the response.
    Subscribe,
    /// Append a user turn and start an assistant completion.
    SendMessage { text: String },
    /// Best-effort cancellation of the in-flight turn.
    Cancel,
    /// Update the persona; the change is persisted immediately.
    SetPersona { name: Option<String> },
    /// Read back the current `SessionState`.
    GetState,
    /// List installed personas (global + project-scoped) so the UI can
    /// render a picker. Returns enough metadata to display the
    /// dropdown without a second round-trip.
    ListPersonas,
    /// Run the agent loop against the existing transcript without
    /// appending a new user message. Used by the "rerun" affordance
    /// in the chat UI when the user wants another assistant pass on
    /// what's already there.
    Rerun,
    /// In-place edit of a single projected history entry. `index`
    /// addresses the UI's projected scrollback (the same `Vec` shape
    /// returned by `Subscribed.history`), not the engine's underlying
    /// `Vec<Message>`. No truncation: editing a mid-history entry
    /// rewrites just that text and leaves later turns alone.
    EditMessage { index: u32, text: String },
    /// Delete a single projected history entry. For an Assistant entry
    /// that shares its underlying `Message::Assistant` with a Thinking
    /// entry, deletion blanks just the assistant text (and the message
    /// is dropped from projection); deleting Thinking nulls the
    /// `thinking` field. Deleting a User entry drops the underlying
    /// `Message::User` outright.
    DeleteMessage { index: u32 },
    /// Truncate everything from the projected entry onward, including
    /// the underlying message that owns it.
    DeleteFromHere { index: u32 },
    /// Fetch the metrics sidecar projected to the same shape as
    /// `Subscribed.history` (one entry per `HistoricalMessage`). New
    /// at variant index 10 — appended to the end so existing indices
    /// stay stable.
    GetMetrics,
    /// Snapshot of the live sub-agent registry. The same data is also
    /// pushed via `ChatEvent::SubAgentsChanged` whenever the engine
    /// notices a transition; this request gives a freshly-mounted
    /// subscriber a starting state without waiting for the next change.
    ListSubAgents,
    /// Read-only snapshot of one sub-agent's transcript. The same data
    /// is also pushed via `ChatEvent::SubAgentTranscriptUpdated`
    /// whenever the child appends — request once on first selection
    /// and let the broadcast keep the view warm.
    GetSubAgentTranscript { id: String },
    /// Replay the persisted reviewer audit log
    /// (`<state_dir>/reviews.jsonl`). Late-joining clients call this
    /// once after `Subscribe`; live updates ride the
    /// `ReviewerCompleted` broadcast.
    ListReviews,
}

pub type ChatResponse = Result<ChatOk, ChatError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatOk {
    /// Subscribed; chrome receives the persisted state plus the
    /// transcript projected to the UI's render shape (no tool calls,
    /// no system, no images — chat-only). Late-joining clients see
    /// the same scrollback any other subscriber would.
    Subscribed {
        state: SessionState,
        history: Vec<HistoricalMessage>,
    },
    MessageQueued { turn_id: TurnId },
    Cancelled,
    StateUpdated { state: SessionState },
    State(SessionState),
    /// Reply to `ListPersonas`.
    Personas { personas: Vec<PersonaInfo> },
    /// Reply to `EditMessage`/`DeleteMessage`/`DeleteFromHere`. The
    /// post-mutation history travels on the `HistoryReplaced`
    /// broadcast — every subscriber (including the originator) reads
    /// state from that single channel rather than racing two copies.
    HistoryAcknowledged,
    /// Reply to `GetMetrics`. Aligned to the same projection that
    /// `Subscribed.history` uses — `metrics[i]` describes the same
    /// `HistoricalMessage` the UI is rendering at index `i`.
    Metrics(Vec<MessageMeta>),
    /// Reply to `ListSubAgents`. Sorted ascending by numeric id so the
    /// UI gets a stable order across snapshots.
    SubAgents(Vec<SubAgentInfo>),
    /// Reply to `GetSubAgentTranscript`. `history` is the same shape as
    /// the parent's `Subscribed.history` — one row per projected entry,
    /// rendered with the same widgets. Empty when the id is unknown
    /// (rather than an error variant — the panel races with `Stop` on
    /// terminal entries and a missing id is interpretable as "gone").
    SubAgentTranscript {
        id: String,
        history: Vec<HistoricalMessage>,
    },
    /// Reply to `ListReviews`. Entries are returned in file order
    /// (i.e. wall-clock order, since the engine appends as
    /// `ReviewerCompleted` events fire).
    Reviews { reviews: Vec<ReviewLogEntry> },
}

/// Wire-shape verdict surfaced to the UI in `ReviewerCompleted`. Mirrors
/// `step::VerdictKind` but lives in the protocol module so the chrome
/// can decode it without depending on engine-internal types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReviewVerdictWire {
    Pass,
    PassWithNit {
        reasoning: String,
    },
    Fail {
        severity: ReviewSeverityWire,
        reasoning: String,
        suggested_fix: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReviewSeverityWire {
    Fix,
    Rethink,
}

/// Terminal disposition of one step's review frame. `Accepted` covers
/// both unanimous-pass and all-skipped outcomes; the chrome only cares
/// that the tool ran. `Rewound` carries the feedback that was forwarded
/// to the prior frame; `Escalated` carries the `on_max_retries=AskUser`
/// reason string surfaced to the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReviewResolution {
    Accepted,
    Rewound { feedback: String },
    Escalated { reason: String },
    /// Review-loop infrastructure failure: a reviewer LLM call
    /// exhausted its retry budget, so the gate could not produce a
    /// sound decision. The turn is aborted; the user must manually
    /// re-engage. Distinct from `Escalated`, which is a *judgment*
    /// outcome (the reviewer asked to escalate); this is the system
    /// itself failing.
    ReviewSystemFailure { principle: String, error: String },
}

/// One row of the persisted reviewer audit log
/// (`<state_dir>/reviews.jsonl`). The engine appends one line per
/// `ReviewerCompleted` event; `ListReviews` replays the whole file so
/// late-joining clients see the full history. `tool_name` and
/// `args_summary` are denormalized from the frame so the sidecar is
/// self-contained — the chrome doesn't have to cross-reference an
/// in-memory frame table.
///
/// `persona` is `Option<String>` so the chrome can synthesize rows
/// from the live `ReviewerCompleted` broadcast (which omits persona
/// to keep the event small and join-via-`ListPrinciples`-friendly)
/// without the empty-string sentinel that used to mean "look it up
/// yourself." `None` = unknown, the chrome should resolve from the
/// principle list; `Some` = recorded at write time on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewLogEntry {
    pub ts: String,
    pub step_id: u64,
    pub reviewer_call_id: u64,
    pub principle: String,
    pub persona: Option<String>,
    pub tool_name: String,
    pub args_summary: String,
    pub verdict: ReviewVerdictWire,
    /// Tool-call id of the attempt that this verdict was scored
    /// against. The UI groups verdicts under each tool bubble by this
    /// id — distinct from `step_id` because a step may have multiple
    /// attempts (different `call_id`s) before reviewers approve.
    /// `None` on rows persisted before this field existed.
    #[serde(default)]
    pub call_id: Option<String>,
}

/// One row in the sub-agent panel. `id` is the canonical display form
/// (`agent#7`) so the UI can render it without re-formatting. `status`
/// is structured rather than stringly so the UI can pick its own
/// styling for each terminal kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentInfo {
    pub id: String,
    /// `None` for top-level children of the main session; `Some(id)`
    /// when one sub-agent spawned this one. Drives tree rendering on
    /// the UI side.
    pub parent_id: Option<String>,
    pub persona: String,
    pub status: SubAgentStatus,
    /// Truncated last assistant-text fragment from the child. `None`
    /// until the first progress event arrives.
    pub last_progress: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubAgentStatus {
    Running,
    Completed,
    Failed { reason: String },
    Stopped,
}

/// Per-projected-entry metrics. One variant per `HistoricalMessage`
/// kind, in declared order — variant index 0 = `User`, 1 = `Assistant`,
/// etc. Each variant carries only the fields its kind can validly
/// produce, so e.g. a `User` entry can't accidentally encode token
/// counts. Timestamps are RFC3339 wrapped in `Option` (`None` =
/// transcript loaded before metrics existed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageMeta {
    User {
        timestamp: Option<String>,
    },
    Assistant {
        timestamp: Option<String>,
        ttft_ms: Option<u64>,
        duration_ms: Option<u64>,
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
    },
    Thinking {
        timestamp: Option<String>,
        ttft_ms: Option<u64>,
        duration_ms: Option<u64>,
    },
    Tool {
        timestamp: Option<String>,
        duration_ms: Option<u64>,
    },
    SubAgentReply {
        timestamp: Option<String>,
    },
    SubAgentFailure {
        timestamp: Option<String>,
    },
    Summary {
        timestamp: Option<String>,
    },
}

/// One row in the persona picker. Sourced from
/// `lutin_entities::Persona::list` then projected to the bare minimum
/// the chat UI needs — full Persona is heavy (system prompt, tool
/// filters, …) and the chrome doesn't render any of it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaInfo {
    /// Filename stem; canonical id used by `SetPersona`.
    pub name: String,
    pub display_name: String,
    /// Empty string if the persona doesn't pin a model. Encoded as
    /// `String` (not `Option<String>`) to keep the postcard layout
    /// simple — empty-as-absent is the same convention used elsewhere
    /// in this protocol.
    pub model: String,
}

/// One entry in the rendered scrollback. The engine projects its full
/// `Vec<lutin_llm::Message>` to this UI-friendly shape on `Subscribe`,
/// preserving original order so tool exchanges interleave with text
/// turns the way the user saw them happen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoricalMessage {
    User(String),
    Assistant(String),
    /// Reasoning / extended-thinking text emitted alongside an assistant
    /// turn. Persisted so re-subscribers see the same conversation that
    /// live listeners saw stream by.
    Thinking(String),
    /// One tool exchange. `arguments_json` is the raw JSON the model
    /// emitted; the TS decoder parses it once at the wire boundary so
    /// downstream code sees a parsed value. `outcome` is `None` for the
    /// mid-turn snapshot case where a call has been emitted but no
    /// result has come back yet.
    Tool {
        call_id: String,
        name: String,
        arguments_json: String,
        outcome: Option<ToolOutcome>,
    },
    /// Successful reply produced by a sub-agent. Rendered with
    /// attribution ("agent#7 said …") rather than as a local user turn.
    SubAgentReply { agent_id: String, text: String },
    /// Sub-agent terminated with a failure; `reason` is the engine's
    /// error string (truncated `AgentUpdate::Failed` payload).
    SubAgentFailure { agent_id: String, reason: String },
    /// Compaction artefact: the model received this single condensed
    /// summary in place of the older messages it covers. The full
    /// pre-summary transcript is archived in
    /// `<state_dir>/compaction_archive.json` for inspection.
    Summary { text: String },
}

/// Result of a tool call. Two variants that carry the result/error text
/// directly — replaces the older `(ok: bool, text: String)` pair where
/// the meaning of `text` flipped on `ok`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolOutcome {
    Ok(String),
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
pub enum ChatError {
    #[error("no turn in progress")]
    NoTurnInFlight,
    #[error("persona not found: {0}")]
    PersonaNotFound(String),
    #[error("provider not configured: {0}")]
    ProviderNotFound(String),
    #[error("provider '{name}' misconfigured: {reason}")]
    ProviderMisconfigured { name: String, reason: String },
    #[error("provider kind unsupported: {0}")]
    ProviderUnsupported(String),
    #[error("internal: {0}")]
    Internal(String),
    #[error("a turn is in flight; cancel it before mutating history")]
    TurnInFlight,
    /// Review loop is active for some step (frame is `Active` on the
    /// step stack). Index-based snapshot rewind requires the transcript
    /// to stay append-only while a frame is active, so transcript
    /// mutations are rejected until the stack settles.
    #[error("review in flight; transcript mutations are blocked until reviewers settle")]
    ReviewInFlight,
    #[error("history index out of range: {0}")]
    HistoryIndexOutOfRange(u32),
    /// The mutation succeeded in memory but the on-disk transcript /
    /// metrics sidecar could not be persisted. `op` names the failing
    /// step ("save transcript" / "save metrics") so the UI can show
    /// the user something more actionable than a blank "internal".
    #[error("persist failed during {op}")]
    PersistFailed { op: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatEvent {
    /// Streaming assistant text delta.
    Delta(String),
    /// Streaming reasoning / thinking delta.
    Reasoning(String),
    /// `arguments_json` is the raw JSON the model emitted as the tool
    /// call's input — the TS decoder parses it once at the wire
    /// boundary so the UI sees a parsed value. Fired once per call,
    /// after `ToolCallStreaming` + zero or more `ToolCallArgsDelta`.
    ToolCallArgsParsed { id: String, name: String, arguments_json: String },
    ToolCallCompleted { id: String, outcome: ToolOutcome },
    /// Terminal event for one turn.
    MessageFinished { turn_id: TurnId, reason: FinishReason },
    /// Pushed when `SessionState` mutates so subscribers can rerender.
    StateChanged(SessionState),
    /// Pushed after `EditMessage`/`DeleteMessage`/`DeleteFromHere`
    /// applies, so every connected subscriber rebuilds its scrollback
    /// from the canonical projected history.
    HistoryReplaced(Vec<HistoricalMessage>),
    /// Pushed alongside `HistoryReplaced` (or after a turn finishes) so
    /// subscribers can rerender per-message footers (timestamp, TTFT,
    /// duration, token counts). Length matches the most-recent
    /// `HistoryReplaced` entry-for-entry.
    MetricsReplaced(Vec<MessageMeta>),
    /// Live sub-agent registry snapshot. Emitted at turn end and on
    /// every terminal sub-agent transition; the UI panel rebinds its
    /// list straight from the payload (no per-id diffing required).
    SubAgentsChanged(Vec<SubAgentInfo>),
    /// One sub-agent's transcript was extended (or rewound, in the
    /// terminal-stamp case). Carries the full projected history — the
    /// UI replaces what it has rather than diffing, matching the
    /// `HistoryReplaced` convention. Open child views subscribe to this
    /// keyed by `id` and ignore deltas for other ids.
    SubAgentTranscriptUpdated {
        id: String,
        history: Vec<HistoricalMessage>,
    },
    /// A new review frame was pushed for `step_id`. Emitted once per
    /// fresh frame; subsequent retries on the same step do not
    /// re-emit (the chrome distinguishes them by `attempt` on
    /// `ReviewFrameProgress`). No `max_attempts` field on purpose:
    /// retry budgets are per-principle, and at frame-open time no
    /// principle has failed yet, so any aggregate would be a guess.
    /// The chrome renders "attempt 1" until the first
    /// `ReviewFrameProgress` carries a real "N/M" pair for the
    /// failing principle.
    ReviewFrameOpened {
        step_id: u64,
        /// Tool-call id of the first attempt (the one that opened the
        /// frame). The UI uses this to anchor the iteration-box
        /// outline to a specific tool bubble.
        call_id: String,
        tool_name: String,
        /// Truncated single-line projection of the tool arguments JSON
        /// (~120 chars). Engine-side truncation keeps the wire small
        /// without forcing the chrome to format raw arg JSON.
        args_summary: String,
    },
    /// One reviewer LLM call started for `(step_id,
    /// reviewer_call_id)`. The chrome uses this as the "row appeared"
    /// cue in the sidebar; the matching `ReviewerCompleted` carries
    /// the verdict. `reviewer_call_id` is a per-session monotonic id
    /// — distinct from the per-principle retry `attempt` carried on
    /// `ReviewFrameProgress`. Persona is not on the event — chrome
    /// joins against `ListPrinciples` to keep persona-at-emit-time
    /// from drifting from the canonical mapping.
    ReviewerStarted {
        step_id: u64,
        /// Tool-call id of the attempt this reviewer is scoring. UI
        /// groups inline verdicts under the matching tool bubble.
        call_id: String,
        reviewer_call_id: u64,
        principle: String,
    },
    /// One reviewer LLM call completed. Re-running the same principle
    /// in a later iteration of the same step gets a fresh
    /// `reviewer_call_id`.
    ReviewerCompleted {
        step_id: u64,
        /// Tool-call id of the attempt this verdict scores. UI keys
        /// inline verdict rows off this id (one bubble = one attempt).
        call_id: String,
        reviewer_call_id: u64,
        principle: String,
        verdict: ReviewVerdictWire,
        /// RFC3339 timestamp captured on the runner side.
        ts: String,
    },
    /// Progress update for an active review frame. Drives the in-chat
    /// "reviewing · attempt N/M · K blocking" indicator. `attempt` is
    /// the count of *failed* iterations so far on the highest-pressure
    /// principle; `max_attempts` mirrors that principle's
    /// `max_retries`. `blocking` lists the principle names that
    /// currently fail.
    ReviewFrameProgress {
        step_id: u64,
        /// Tool-call id of the attempt that just got denied. UI marks
        /// that bubble's status with the failure summary.
        call_id: String,
        attempt: u32,
        max_attempts: u32,
        blocking: Vec<String>,
    },
    /// Terminal event for one frame: tool ran, frame rewound, or
    /// escalated to the user. Pairs with the `ReviewFrameOpened` sent
    /// when the frame first appeared.
    ReviewFrameResolved {
        step_id: u64,
        /// Tool-call id of the resolving attempt — accepted/escalated
        /// → the call that ran (or would have); rewound → the call
        /// the agent voluntarily backed out of; review-system failure
        /// → the call the failing reviewer was scoring. Always a real
        /// id; never empty.
        call_id: String,
        outcome: ReviewResolution,
    },
    /// Provider opened a tool-call block; arguments are about to
    /// stream in via `ToolCallArgsDelta`. The UI can render an
    /// in-progress placeholder keyed by `id`. Appended at the end of
    /// the variant list so existing postcard tags stay stable.
    ToolCallStreaming { id: String, name: String },
    /// Incremental fragment of the tool call's arguments JSON. Fired
    /// as the LLM streams the call's input; concatenating fragments
    /// in order yields the raw text that `ToolCallArgsParsed` later
    /// delivers as parsed JSON.
    ToolCallArgsDelta { id: String, args: String },
    /// Live tick of the running session summary — emitted on every
    /// provider usage report (one per agent-loop iteration) and at
    /// turn boundaries. `context_tokens` is the most recent prompt
    /// size (current context fill); the totals are cumulative across
    /// the lifetime of the session, kept monotonic mid-turn by
    /// adding the current usage on top of the pre-turn baseline.
    /// Appended at the end of the variant list so existing postcard
    /// tags stay stable.
    SummaryUpdated {
        context_tokens: Option<u32>,
        total_prompt_tokens: u64,
        total_completion_tokens: u64,
    },
    /// One or more denied attempts in a now-resolved step are no
    /// longer in the canonical transcript. The UI drops every tool
    /// entry whose `call_id` matches; the engine emits this *before*
    /// the matching `ReviewFrameResolved` so the iteration-box outline
    /// is still anchored when the squash hits.
    ///
    /// Mirrors what end-of-turn `squash_denied_attempts` removes from
    /// the message log. Live emission lets the chat scrollback shed
    /// failed bubbles the moment a step accepts (or rewinds), instead
    /// of the user seeing a clutter of red tool entries persist until
    /// the entire turn is over.
    AttemptsSquashed {
        call_ids: Vec<String>,
    },
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

/// Re-exported so callers don't need a direct `lutin-workflow-sdk` dep
/// just to log the canonical path. Backed by [`sdk_state::state_path`].
pub fn state_path(state_dir: &Path) -> PathBuf {
    sdk_state::state_path(state_dir)
}

pub type StateError = sdk_state::StateError;

/// Load `SessionState` from `<state_dir>/state.toml`. Returns
/// `Default::default()` if the file is missing.
pub fn load_state(state_dir: &Path) -> Result<SessionState, StateError> {
    sdk_state::load(state_dir)
}

/// Persist `SessionState` to `<state_dir>/state.toml`.
pub fn save_state(state_dir: &Path, state: &SessionState) -> Result<(), StateError> {
    sdk_state::save(state_dir, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn request_roundtrip() {
        let r = ChatRequest::SendMessage { text: "hi".into() };
        assert_eq!(decode::<ChatRequest>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn event_roundtrip() {
        let e = ChatEvent::MessageFinished {
            turn_id: TurnId(7),
            reason: FinishReason::Completed,
        };
        assert_eq!(decode::<ChatEvent>(&encode(&e).unwrap()).unwrap(), e);
    }

    #[test]
    fn state_default_when_missing() {
        let tmp = TempDir::new().unwrap();
        let s = load_state(tmp.path()).unwrap();
        assert_eq!(s, SessionState::default());
    }

    #[test]
    fn empty_strings_deserialize_as_none() {
        // Boundary invariant: `Some("")` collapses to `None` so downstream
        // code never has to defend against blank-but-present strings.
        let s: SessionState = toml::from_str("persona = \"\"\nmodel_override = \"\"\n").unwrap();
        assert_eq!(s.persona, None);
        assert_eq!(s.model_override, None);
    }

    #[test]
    fn provider_misconfigured_roundtrip() {
        let e: ChatResponse = Err(ChatError::ProviderMisconfigured {
            name: "anthropic".into(),
            reason: "env var unset".into(),
        });
        assert_eq!(decode::<ChatResponse>(&encode(&e).unwrap()).unwrap(), e);
    }

    /// Golden bytes pinned against the JS postcard codec in
    /// `workflows/chat/ui/src/postcard.ts` + `chat.ts`. Any change here
    /// is a breaking change to the iframe's decoder; mirror it on the
    /// JS side in the matching `golden_bytes` table before merging.
    #[test]
    fn golden_postcard_bytes() {
        let cases: &[(&str, Vec<u8>)] = &[
            ("ChatRequest::Subscribe", encode(&ChatRequest::Subscribe).unwrap()),
            (
                "ChatRequest::SendMessage{hi}",
                encode(&ChatRequest::SendMessage { text: "hi".into() }).unwrap(),
            ),
            ("ChatRequest::Cancel", encode(&ChatRequest::Cancel).unwrap()),
            (
                "ChatRequest::SetPersona(None)",
                encode(&ChatRequest::SetPersona { name: None }).unwrap(),
            ),
            (
                "ChatRequest::SetPersona(Some(\"alice\"))",
                encode(&ChatRequest::SetPersona { name: Some("alice".into()) }).unwrap(),
            ),
            ("ChatRequest::Rerun", encode(&ChatRequest::Rerun).unwrap()),
            (
                "ChatEvent::Delta(\"hi\")",
                encode(&ChatEvent::Delta("hi".into())).unwrap(),
            ),
            (
                "ChatEvent::MessageFinished{7, Completed}",
                encode(&ChatEvent::MessageFinished {
                    turn_id: TurnId(7),
                    reason: FinishReason::Completed,
                })
                .unwrap(),
            ),
            (
                "ChatEvent::MessageFinished{300, Failed(\"boom\")}",
                encode(&ChatEvent::MessageFinished {
                    turn_id: TurnId(300),
                    reason: FinishReason::Failed("boom".into()),
                })
                .unwrap(),
            ),
            (
                "ChatResponse::Ok(Subscribed{empty})",
                encode::<ChatResponse>(&Ok(ChatOk::Subscribed {
                    state: SessionState::default(),
                    history: vec![],
                }))
                .unwrap(),
            ),
            (
                "ChatResponse::Ok(Subscribed{persona,1msg})",
                encode::<ChatResponse>(&Ok(ChatOk::Subscribed {
                    state: SessionState {
                        persona: Some("alice".into()),
                        model_override: None,
                        review_concurrency: None,
                    },
                    history: vec![HistoricalMessage::User("hi".into())],
                }))
                .unwrap(),
            ),
            (
                "ChatResponse::Err(NoTurnInFlight)",
                encode::<ChatResponse>(&Err(ChatError::NoTurnInFlight)).unwrap(),
            ),
            (
                "ChatRequest::EditMessage{3,\"hi\"}",
                encode(&ChatRequest::EditMessage { index: 3, text: "hi".into() }).unwrap(),
            ),
            (
                "ChatRequest::DeleteMessage{2}",
                encode(&ChatRequest::DeleteMessage { index: 2 }).unwrap(),
            ),
            (
                "ChatRequest::DeleteFromHere{1}",
                encode(&ChatRequest::DeleteFromHere { index: 1 }).unwrap(),
            ),
            (
                "ChatEvent::HistoryReplaced(empty)",
                encode(&ChatEvent::HistoryReplaced(vec![])).unwrap(),
            ),
            (
                "ChatResponse::Ok(HistoryAcknowledged)",
                encode::<ChatResponse>(&Ok(ChatOk::HistoryAcknowledged)).unwrap(),
            ),
            ("ChatRequest::GetMetrics", encode(&ChatRequest::GetMetrics).unwrap()),
            (
                "ChatResponse::Ok(Metrics(empty))",
                encode::<ChatResponse>(&Ok(ChatOk::Metrics(vec![]))).unwrap(),
            ),
            (
                "ChatResponse::Ok(Metrics(1user))",
                encode::<ChatResponse>(&Ok(ChatOk::Metrics(vec![MessageMeta::User {
                    timestamp: Some("T".into()),
                }])))
                .unwrap(),
            ),
            (
                "ChatEvent::MetricsReplaced(empty)",
                encode(&ChatEvent::MetricsReplaced(vec![])).unwrap(),
            ),
            (
                "ChatEvent::ReviewFrameOpened",
                encode(&ChatEvent::ReviewFrameOpened {
                    step_id: 7,
                    call_id: "c".into(),
                    tool_name: "edit".into(),
                    args_summary: String::new(),
                })
                .unwrap(),
            ),
            (
                "ChatEvent::ReviewerStarted",
                encode(&ChatEvent::ReviewerStarted {
                    step_id: 7,
                    call_id: "c".into(),
                    reviewer_call_id: 1,
                    principle: "p".into(),
                })
                .unwrap(),
            ),
            (
                "ChatEvent::ReviewerCompleted(Pass)",
                encode(&ChatEvent::ReviewerCompleted {
                    step_id: 7,
                    call_id: "c".into(),
                    reviewer_call_id: 1,
                    principle: "p".into(),
                    verdict: ReviewVerdictWire::Pass,
                    ts: "T".into(),
                })
                .unwrap(),
            ),
            (
                "ChatEvent::ReviewerCompleted(Fail{Fix})",
                encode(&ChatEvent::ReviewerCompleted {
                    step_id: 7,
                    call_id: "c".into(),
                    reviewer_call_id: 2,
                    principle: "p".into(),
                    verdict: ReviewVerdictWire::Fail {
                        severity: ReviewSeverityWire::Fix,
                        reasoning: "no".into(),
                        suggested_fix: None,
                    },
                    ts: "T".into(),
                })
                .unwrap(),
            ),
            (
                "ChatEvent::ReviewFrameProgress",
                encode(&ChatEvent::ReviewFrameProgress {
                    step_id: 7,
                    call_id: "c".into(),
                    attempt: 1,
                    max_attempts: 3,
                    blocking: vec!["p".into()],
                })
                .unwrap(),
            ),
            (
                "ChatEvent::ReviewFrameResolved(Accepted)",
                encode(&ChatEvent::ReviewFrameResolved {
                    step_id: 7,
                    call_id: "c".into(),
                    outcome: ReviewResolution::Accepted,
                })
                .unwrap(),
            ),
            (
                "ChatEvent::ReviewFrameResolved(Rewound)",
                encode(&ChatEvent::ReviewFrameResolved {
                    step_id: 7,
                    call_id: "c".into(),
                    outcome: ReviewResolution::Rewound { feedback: "fb".into() },
                })
                .unwrap(),
            ),
            ("ChatRequest::ListReviews", encode(&ChatRequest::ListReviews).unwrap()),
            (
                "ChatResponse::Ok(Reviews(empty))",
                encode::<ChatResponse>(&Ok(ChatOk::Reviews { reviews: vec![] })).unwrap(),
            ),
            (
                "ChatResponse::Ok(Reviews(1row))",
                encode::<ChatResponse>(&Ok(ChatOk::Reviews {
                    reviews: vec![ReviewLogEntry {
                        ts: "T".into(),
                        step_id: 7,
                        reviewer_call_id: 1,
                        principle: "p".into(),
                        persona: Some("r".into()),
                        tool_name: "edit".into(),
                        args_summary: String::new(),
                        verdict: ReviewVerdictWire::Pass,
                        call_id: Some("c".into()),
                    }],
                }))
                .unwrap(),
            ),
        ];

        let expected: &[(&str, &[u8])] = &[
            ("ChatRequest::Subscribe", &[0x00]),
            ("ChatRequest::SendMessage{hi}", &[0x01, 0x02, b'h', b'i']),
            ("ChatRequest::Cancel", &[0x02]),
            ("ChatRequest::SetPersona(None)", &[0x03, 0x00]),
            (
                "ChatRequest::SetPersona(Some(\"alice\"))",
                &[0x03, 0x01, 0x05, b'a', b'l', b'i', b'c', b'e'],
            ),
            ("ChatRequest::Rerun", &[0x06]),
            ("ChatEvent::Delta(\"hi\")", &[0x00, 0x02, b'h', b'i']),
            ("ChatEvent::MessageFinished{7, Completed}", &[0x04, 0x07, 0x00]),
            (
                "ChatEvent::MessageFinished{300, Failed(\"boom\")}",
                &[0x04, 0xac, 0x02, 0x02, 0x04, b'b', b'o', b'o', b'm'],
            ),
            (
                "ChatResponse::Ok(Subscribed{empty})",
                &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            ),
            (
                "ChatResponse::Ok(Subscribed{persona,1msg})",
                &[
                    0x00, 0x00, // Ok, Subscribed
                    0x01, 0x05, b'a', b'l', b'i', b'c', b'e', // Some("alice")
                    0x00, // model_override None
                    0x00, // review_concurrency None
                    0x01, // history len 1
                    0x00, // role User
                    0x02, b'h', b'i', // text "hi"
                ],
            ),
            ("ChatResponse::Err(NoTurnInFlight)", &[0x01, 0x00]),
            (
                "ChatRequest::EditMessage{3,\"hi\"}",
                &[0x07, 0x03, 0x02, b'h', b'i'],
            ),
            ("ChatRequest::DeleteMessage{2}", &[0x08, 0x02]),
            ("ChatRequest::DeleteFromHere{1}", &[0x09, 0x01]),
            ("ChatEvent::HistoryReplaced(empty)", &[0x06, 0x00]),
            ("ChatResponse::Ok(HistoryAcknowledged)", &[0x00, 0x06]),
            ("ChatRequest::GetMetrics", &[0x0a]),
            ("ChatResponse::Ok(Metrics(empty))", &[0x00, 0x07, 0x00]),
            (
                "ChatResponse::Ok(Metrics(1user))",
                &[
                    0x00, 0x07, // Ok, Metrics
                    0x01, // vec len 1
                    0x00, // MessageMeta::User variant
                    0x01, 0x01, b'T', // timestamp = Some("T")
                ],
            ),
            ("ChatEvent::MetricsReplaced(empty)", &[0x07, 0x00]),
            (
                "ChatEvent::ReviewFrameOpened",
                &[
                    0x0a, // variant 10
                    0x07, // step_id 7
                    0x01, b'c', // call_id "c"
                    0x04, b'e', b'd', b'i', b't', // tool_name "edit"
                    0x00, // args_summary ""
                ],
            ),
            (
                "ChatEvent::ReviewerStarted",
                &[
                    0x0b, // variant 11
                    0x07, // step_id 7
                    0x01, b'c', // call_id "c"
                    0x01, // reviewer_call_id 1
                    0x01, b'p', // principle "p"
                ],
            ),
            (
                "ChatEvent::ReviewerCompleted(Pass)",
                &[
                    0x0c, // variant 12
                    0x07, // step_id
                    0x01, b'c', // call_id "c"
                    0x01, // reviewer_call_id
                    0x01, b'p', // principle
                    0x00, // verdict variant Pass
                    0x01, b'T', // ts "T"
                ],
            ),
            (
                "ChatEvent::ReviewerCompleted(Fail{Fix})",
                &[
                    0x0c, // variant 12
                    0x07, // step_id
                    0x01, b'c', // call_id "c"
                    0x02, // reviewer_call_id
                    0x01, b'p', // principle
                    0x02, // verdict variant Fail
                    0x00, // severity Fix
                    0x02, b'n', b'o', // reasoning "no"
                    0x00, // suggested_fix None
                    0x01, b'T', // ts "T"
                ],
            ),
            (
                "ChatEvent::ReviewFrameProgress",
                &[
                    0x0d, // variant 13
                    0x07, // step_id
                    0x01, b'c', // call_id "c"
                    0x01, 0x03, // attempt, max_attempts
                    0x01, 0x01, b'p', // blocking ["p"]
                ],
            ),
            (
                "ChatEvent::ReviewFrameResolved(Accepted)",
                &[
                    0x0e, // variant 14
                    0x07, // step_id
                    0x01, b'c', // call_id "c"
                    0x00, // outcome Accepted
                ],
            ),
            (
                "ChatEvent::ReviewFrameResolved(Rewound)",
                &[
                    0x0e, // variant 14
                    0x07, // step_id
                    0x01, b'c', // call_id "c"
                    0x01, // outcome Rewound
                    0x02, b'f', b'b', // feedback "fb"
                ],
            ),
            ("ChatRequest::ListReviews", &[0x0d]),
            ("ChatResponse::Ok(Reviews(empty))", &[0x00, 0x0a, 0x00]),
            (
                "ChatResponse::Ok(Reviews(1row))",
                &[
                    0x00, 0x0a, // Ok, Reviews
                    0x01, // vec len 1
                    0x01, b'T', // ts "T"
                    0x07, // step_id 7
                    0x01, // reviewer_call_id 1
                    0x01, b'p', // principle "p"
                    0x01, // persona Option tag = Some
                    0x01, b'r', // persona "r"
                    0x04, b'e', b'd', b'i', b't', // tool_name "edit"
                    0x00, // args_summary ""
                    0x00, // verdict variant Pass
                    0x01, // call_id Option tag = Some
                    0x01, b'c', // call_id "c"
                ],
            ),
        ];

        assert_eq!(cases.len(), expected.len());
        for ((label, got), (_, want)) in cases.iter().zip(expected.iter()) {
            assert_eq!(got.as_slice(), *want, "case {label}");
        }
    }

    #[test]
    fn state_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let s = SessionState {
            persona: Some("assistant".into()),
            model_override: None,
            review_concurrency: None,
        };
        save_state(tmp.path(), &s).unwrap();
        assert_eq!(load_state(tmp.path()).unwrap(), s);
    }

    /// Exhaustive snapshot of every wire-visible variant. The Rust
    /// side is the authority: this test constructs one canonical
    /// instance per variant, serializes it, and writes the result to
    /// `ui/wire-corpus.json`. The TS test in
    /// `workflows/principled/ui/src/wire-corpus.test.ts` reads the
    /// same file and decodes every entry, so any Rust-side variant
    /// addition (or reordering) that the JS decoder hasn't been
    /// updated for fails loudly at test time instead of as a runtime
    /// `postcard: unexpected EOF` in the chrome.
    ///
    /// Coverage is enforced at compile time: each `variant_tag` match
    /// below has no `_` arm, so adding a new variant to any wire enum
    /// breaks compilation here. The matching `cases` table must then
    /// be extended to construct the new variant.
    ///
    /// Re-roll after intentional wire changes by setting
    /// `WIRE_CORPUS_REGEN=1`. Otherwise the test fails (after
    /// rewriting the file) so CI catches drift.
    #[test]
    fn wire_corpus_in_sync() {
        #[derive(Serialize)]
        struct Entry {
            name: String,
            channel: &'static str,
            kind: &'static str,
            hex: String,
        }
        let mut entries: Vec<Entry> = Vec::new();
        let push_req = |entries: &mut Vec<Entry>, name: &str, r: ChatRequest| {
            let kind = chat_request_tag(&r);
            entries.push(Entry {
                name: format!("ChatRequest::{name}"),
                channel: "request",
                kind,
                hex: hex_encode(&encode(&r).unwrap()),
            });
        };
        let push_ok = |entries: &mut Vec<Entry>, name: &str, ok: ChatOk| {
            let kind = chat_ok_tag(&ok);
            let resp: ChatResponse = Ok(ok);
            entries.push(Entry {
                name: format!("ChatOk::{name}"),
                channel: "response",
                kind,
                hex: hex_encode(&encode(&resp).unwrap()),
            });
        };
        let push_err = |entries: &mut Vec<Entry>, name: &str, err: ChatError| {
            let kind = chat_error_tag(&err);
            let resp: ChatResponse = Err(err);
            entries.push(Entry {
                name: format!("ChatError::{name}"),
                channel: "response",
                kind,
                hex: hex_encode(&encode(&resp).unwrap()),
            });
        };
        let push_evt = |entries: &mut Vec<Entry>, name: &str, e: ChatEvent| {
            let kind = chat_event_tag(&e);
            entries.push(Entry {
                name: format!("ChatEvent::{name}"),
                channel: "event",
                kind,
                hex: hex_encode(&encode(&e).unwrap()),
            });
        };

        // ── ChatRequest ───────────────────────────────────────────
        push_req(&mut entries, "Subscribe", ChatRequest::Subscribe);
        push_req(
            &mut entries,
            "SendMessage",
            ChatRequest::SendMessage { text: "hi".into() },
        );
        push_req(&mut entries, "Cancel", ChatRequest::Cancel);
        push_req(
            &mut entries,
            "SetPersona",
            ChatRequest::SetPersona { name: Some("alice".into()) },
        );
        push_req(&mut entries, "GetState", ChatRequest::GetState);
        push_req(&mut entries, "ListPersonas", ChatRequest::ListPersonas);
        push_req(&mut entries, "Rerun", ChatRequest::Rerun);
        push_req(
            &mut entries,
            "EditMessage",
            ChatRequest::EditMessage { index: 3, text: "hi".into() },
        );
        push_req(
            &mut entries,
            "DeleteMessage",
            ChatRequest::DeleteMessage { index: 2 },
        );
        push_req(
            &mut entries,
            "DeleteFromHere",
            ChatRequest::DeleteFromHere { index: 1 },
        );
        push_req(&mut entries, "GetMetrics", ChatRequest::GetMetrics);
        push_req(&mut entries, "ListSubAgents", ChatRequest::ListSubAgents);
        push_req(
            &mut entries,
            "GetSubAgentTranscript",
            ChatRequest::GetSubAgentTranscript { id: "agent#1".into() },
        );
        push_req(&mut entries, "ListReviews", ChatRequest::ListReviews);

        // ── ChatOk ────────────────────────────────────────────────
        let one_state = SessionState::default();
        push_ok(
            &mut entries,
            "Subscribed",
            ChatOk::Subscribed {
                state: one_state.clone(),
                history: vec![HistoricalMessage::User("hi".into())],
            },
        );
        push_ok(
            &mut entries,
            "MessageQueued",
            ChatOk::MessageQueued { turn_id: TurnId(7) },
        );
        push_ok(&mut entries, "Cancelled", ChatOk::Cancelled);
        push_ok(
            &mut entries,
            "StateUpdated",
            ChatOk::StateUpdated { state: one_state.clone() },
        );
        push_ok(&mut entries, "State", ChatOk::State(one_state.clone()));
        push_ok(
            &mut entries,
            "Personas",
            ChatOk::Personas {
                personas: vec![PersonaInfo {
                    name: "p".into(),
                    display_name: "P".into(),
                    model: String::new(),
                }],
            },
        );
        push_ok(&mut entries, "HistoryAcknowledged", ChatOk::HistoryAcknowledged);
        // Embed every MessageMeta variant so the TS readMessageMeta
        // decoder is exercised end-to-end.
        push_ok(&mut entries, "Metrics", ChatOk::Metrics(all_message_metas()));
        push_ok(
            &mut entries,
            "SubAgents",
            ChatOk::SubAgents(vec![SubAgentInfo {
                id: "agent#1".into(),
                parent_id: None,
                persona: "coder".into(),
                status: SubAgentStatus::Running,
                last_progress: None,
            }]),
        );
        // Embed every HistoricalMessage variant so readHistoricalMessage
        // is exercised end-to-end (Summary at variant 6 was the
        // missing case that motivated this test).
        push_ok(
            &mut entries,
            "SubAgentTranscript",
            ChatOk::SubAgentTranscript {
                id: "agent#1".into(),
                history: all_historical_messages(),
            },
        );
        push_ok(
            &mut entries,
            "Reviews",
            ChatOk::Reviews {
                reviews: vec![ReviewLogEntry {
                    ts: "T".into(),
                    step_id: 7,
                    reviewer_call_id: 1,
                    principle: "p".into(),
                    persona: Some("r".into()),
                    tool_name: "edit".into(),
                    args_summary: String::new(),
                    verdict: ReviewVerdictWire::Pass,
                    call_id: Some("c".into()),
                }],
            },
        );

        // ── ChatError ─────────────────────────────────────────────
        push_err(&mut entries, "NoTurnInFlight", ChatError::NoTurnInFlight);
        push_err(
            &mut entries,
            "PersonaNotFound",
            ChatError::PersonaNotFound("x".into()),
        );
        push_err(
            &mut entries,
            "ProviderNotFound",
            ChatError::ProviderNotFound("x".into()),
        );
        push_err(
            &mut entries,
            "ProviderMisconfigured",
            ChatError::ProviderMisconfigured {
                name: "x".into(),
                reason: "y".into(),
            },
        );
        push_err(
            &mut entries,
            "ProviderUnsupported",
            ChatError::ProviderUnsupported("x".into()),
        );
        push_err(&mut entries, "Internal", ChatError::Internal("oops".into()));
        push_err(&mut entries, "TurnInFlight", ChatError::TurnInFlight);
        push_err(&mut entries, "ReviewInFlight", ChatError::ReviewInFlight);
        push_err(
            &mut entries,
            "HistoryIndexOutOfRange",
            ChatError::HistoryIndexOutOfRange(5),
        );
        push_err(
            &mut entries,
            "PersistFailed",
            ChatError::PersistFailed { op: "save".into() },
        );

        // ── ChatEvent ─────────────────────────────────────────────
        push_evt(&mut entries, "Delta", ChatEvent::Delta("hi".into()));
        push_evt(&mut entries, "Reasoning", ChatEvent::Reasoning("th".into()));
        push_evt(
            &mut entries,
            "ToolCallArgsParsed",
            ChatEvent::ToolCallArgsParsed {
                id: "c".into(),
                name: "edit".into(),
                arguments_json: "{}".into(),
            },
        );
        push_evt(
            &mut entries,
            "ToolCallCompleted",
            ChatEvent::ToolCallCompleted {
                id: "c".into(),
                outcome: ToolOutcome::Ok("done".into()),
            },
        );
        push_evt(
            &mut entries,
            "MessageFinished",
            ChatEvent::MessageFinished {
                turn_id: TurnId(7),
                reason: FinishReason::Completed,
            },
        );
        push_evt(
            &mut entries,
            "StateChanged",
            ChatEvent::StateChanged(one_state.clone()),
        );
        push_evt(
            &mut entries,
            "HistoryReplaced",
            ChatEvent::HistoryReplaced(all_historical_messages()),
        );
        push_evt(
            &mut entries,
            "MetricsReplaced",
            ChatEvent::MetricsReplaced(all_message_metas()),
        );
        push_evt(
            &mut entries,
            "SubAgentsChanged",
            ChatEvent::SubAgentsChanged(vec![SubAgentInfo {
                id: "agent#1".into(),
                parent_id: Some("agent#0".into()),
                persona: "coder".into(),
                status: SubAgentStatus::Failed { reason: "boom".into() },
                last_progress: Some("…".into()),
            }]),
        );
        push_evt(
            &mut entries,
            "SubAgentTranscriptUpdated",
            ChatEvent::SubAgentTranscriptUpdated {
                id: "agent#1".into(),
                history: vec![HistoricalMessage::Assistant("hello".into())],
            },
        );
        push_evt(
            &mut entries,
            "ReviewFrameOpened",
            ChatEvent::ReviewFrameOpened {
                step_id: 7,
                call_id: "c".into(),
                tool_name: "edit".into(),
                args_summary: String::new(),
            },
        );
        push_evt(
            &mut entries,
            "ReviewerStarted",
            ChatEvent::ReviewerStarted {
                step_id: 7,
                call_id: "c".into(),
                reviewer_call_id: 1,
                principle: "p".into(),
            },
        );
        push_evt(
            &mut entries,
            "ReviewerCompleted",
            ChatEvent::ReviewerCompleted {
                step_id: 7,
                call_id: "c".into(),
                reviewer_call_id: 2,
                principle: "p".into(),
                verdict: ReviewVerdictWire::Fail {
                    severity: ReviewSeverityWire::Fix,
                    reasoning: "no".into(),
                    suggested_fix: None,
                },
                ts: "T".into(),
            },
        );
        push_evt(
            &mut entries,
            "ReviewFrameProgress",
            ChatEvent::ReviewFrameProgress {
                step_id: 7,
                call_id: "c".into(),
                attempt: 1,
                max_attempts: 3,
                blocking: vec!["p".into()],
            },
        );
        push_evt(
            &mut entries,
            "ReviewFrameResolved",
            ChatEvent::ReviewFrameResolved {
                step_id: 7,
                call_id: "c".into(),
                outcome: ReviewResolution::Rewound { feedback: "fb".into() },
            },
        );
        push_evt(
            &mut entries,
            "ToolCallStreaming",
            ChatEvent::ToolCallStreaming {
                id: "c".into(),
                name: "edit".into(),
            },
        );
        push_evt(
            &mut entries,
            "ToolCallArgsDelta",
            ChatEvent::ToolCallArgsDelta {
                id: "c".into(),
                args: "{".into(),
            },
        );
        push_evt(
            &mut entries,
            "SummaryUpdated",
            ChatEvent::SummaryUpdated {
                context_tokens: Some(1024),
                total_prompt_tokens: 5_000,
                total_completion_tokens: 250,
            },
        );
        push_evt(
            &mut entries,
            "AttemptsSquashed",
            ChatEvent::AttemptsSquashed { call_ids: vec!["c1".into(), "c2".into()] },
        );

        let json = serde_json::to_string_pretty(&entries).unwrap() + "\n";
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/wire-corpus.json");
        if std::env::var("WIRE_CORPUS_REGEN").is_ok() {
            std::fs::write(&path, &json).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        if on_disk != json {
            std::fs::write(&path, &json).unwrap();
            panic!(
                "wire-corpus.json was out of date — regenerated at {}. \
                 Commit the change (or set WIRE_CORPUS_REGEN=1 to skip the assert).",
                path.display()
            );
        }
    }

    /// Hex-encode bytes in lowercase, no separators. Mirrors the format
    /// the bun-side test parses with `parseInt(byte, 16)`.
    fn hex_encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    /// One instance of every `HistoricalMessage` variant, in declared
    /// variant order. Keep in sync with the enum — a new variant
    /// breaks `historical_message_tag`'s exhaustive match below.
    fn all_historical_messages() -> Vec<HistoricalMessage> {
        let v = vec![
            HistoricalMessage::User("u".into()),
            HistoricalMessage::Assistant("a".into()),
            HistoricalMessage::Thinking("t".into()),
            HistoricalMessage::Tool {
                call_id: "c".into(),
                name: "edit".into(),
                arguments_json: "{}".into(),
                outcome: Some(ToolOutcome::Ok("ok".into())),
            },
            HistoricalMessage::SubAgentReply {
                agent_id: "agent#1".into(),
                text: "r".into(),
            },
            HistoricalMessage::SubAgentFailure {
                agent_id: "agent#1".into(),
                reason: "boom".into(),
            },
            HistoricalMessage::Summary { text: "summary".into() },
        ];
        // Compile-time exhaustiveness probe — adding a variant breaks here.
        for h in &v {
            let _ = historical_message_tag(h);
        }
        v
    }

    fn all_message_metas() -> Vec<MessageMeta> {
        let v = vec![
            MessageMeta::User { timestamp: Some("T".into()) },
            MessageMeta::Assistant {
                timestamp: Some("T".into()),
                ttft_ms: Some(100),
                duration_ms: Some(500),
                prompt_tokens: Some(10),
                completion_tokens: Some(20),
            },
            MessageMeta::Thinking {
                timestamp: Some("T".into()),
                ttft_ms: Some(50),
                duration_ms: Some(120),
            },
            MessageMeta::Tool {
                timestamp: Some("T".into()),
                duration_ms: Some(40),
            },
            MessageMeta::SubAgentReply { timestamp: Some("T".into()) },
            MessageMeta::SubAgentFailure { timestamp: Some("T".into()) },
            MessageMeta::Summary { timestamp: Some("T".into()) },
        ];
        for m in &v {
            let _ = message_meta_tag(m);
        }
        v
    }

    // ── Compile-time exhaustiveness probes ───────────────────────
    //
    // Each `*_tag` returns the variant name as a string; the `match`
    // body has no `_` arm, so adding a new variant breaks compilation
    // here. That's the contract that keeps the corpus exhaustive
    // without hand-maintained "did you forget anything?" docs.

    fn chat_request_tag(r: &ChatRequest) -> &'static str {
        match r {
            ChatRequest::Subscribe => "subscribe",
            ChatRequest::SendMessage { .. } => "sendMessage",
            ChatRequest::Cancel => "cancel",
            ChatRequest::SetPersona { .. } => "setPersona",
            ChatRequest::GetState => "getState",
            ChatRequest::ListPersonas => "listPersonas",
            ChatRequest::Rerun => "rerun",
            ChatRequest::EditMessage { .. } => "editMessage",
            ChatRequest::DeleteMessage { .. } => "deleteMessage",
            ChatRequest::DeleteFromHere { .. } => "deleteFromHere",
            ChatRequest::GetMetrics => "getMetrics",
            ChatRequest::ListSubAgents => "listSubAgents",
            ChatRequest::GetSubAgentTranscript { .. } => "getSubAgentTranscript",
            ChatRequest::ListReviews => "listReviews",
        }
    }

    fn chat_ok_tag(ok: &ChatOk) -> &'static str {
        match ok {
            ChatOk::Subscribed { .. } => "subscribed",
            ChatOk::MessageQueued { .. } => "messageQueued",
            ChatOk::Cancelled => "cancelled",
            ChatOk::StateUpdated { .. } => "stateUpdated",
            ChatOk::State(_) => "state",
            ChatOk::Personas { .. } => "personas",
            ChatOk::HistoryAcknowledged => "historyAcknowledged",
            ChatOk::Metrics(_) => "metrics",
            ChatOk::SubAgents(_) => "subAgents",
            ChatOk::SubAgentTranscript { .. } => "subAgentTranscript",
            ChatOk::Reviews { .. } => "reviews",
        }
    }

    fn chat_error_tag(e: &ChatError) -> &'static str {
        match e {
            ChatError::NoTurnInFlight => "noTurnInFlight",
            ChatError::PersonaNotFound(_) => "personaNotFound",
            ChatError::ProviderNotFound(_) => "providerNotFound",
            ChatError::ProviderMisconfigured { .. } => "providerMisconfigured",
            ChatError::ProviderUnsupported(_) => "providerUnsupported",
            ChatError::Internal(_) => "internal",
            ChatError::TurnInFlight => "turnInFlight",
            ChatError::ReviewInFlight => "reviewInFlight",
            ChatError::HistoryIndexOutOfRange(_) => "historyIndexOutOfRange",
            ChatError::PersistFailed { .. } => "persistFailed",
        }
    }

    fn chat_event_tag(e: &ChatEvent) -> &'static str {
        match e {
            ChatEvent::Delta(_) => "delta",
            ChatEvent::Reasoning(_) => "reasoning",
            ChatEvent::ToolCallArgsParsed { .. } => "toolCallArgsParsed",
            ChatEvent::ToolCallCompleted { .. } => "toolCallCompleted",
            ChatEvent::MessageFinished { .. } => "messageFinished",
            ChatEvent::StateChanged(_) => "stateChanged",
            ChatEvent::HistoryReplaced(_) => "historyReplaced",
            ChatEvent::MetricsReplaced(_) => "metricsReplaced",
            ChatEvent::SubAgentsChanged(_) => "subAgentsChanged",
            ChatEvent::SubAgentTranscriptUpdated { .. } => "subAgentTranscriptUpdated",
            ChatEvent::ReviewFrameOpened { .. } => "reviewFrameOpened",
            ChatEvent::ReviewerStarted { .. } => "reviewerStarted",
            ChatEvent::ReviewerCompleted { .. } => "reviewerCompleted",
            ChatEvent::ReviewFrameProgress { .. } => "reviewFrameProgress",
            ChatEvent::ReviewFrameResolved { .. } => "reviewFrameResolved",
            ChatEvent::ToolCallStreaming { .. } => "toolCallStreaming",
            ChatEvent::ToolCallArgsDelta { .. } => "toolCallArgsDelta",
            ChatEvent::SummaryUpdated { .. } => "summaryUpdated",
            ChatEvent::AttemptsSquashed { .. } => "attemptsSquashed",
        }
    }

    fn historical_message_tag(h: &HistoricalMessage) -> &'static str {
        match h {
            HistoricalMessage::User(_) => "user",
            HistoricalMessage::Assistant(_) => "assistant",
            HistoricalMessage::Thinking(_) => "thinking",
            HistoricalMessage::Tool { .. } => "tool",
            HistoricalMessage::SubAgentReply { .. } => "subAgentReply",
            HistoricalMessage::SubAgentFailure { .. } => "subAgentFailure",
            HistoricalMessage::Summary { .. } => "summary",
        }
    }

    fn message_meta_tag(m: &MessageMeta) -> &'static str {
        match m {
            MessageMeta::User { .. } => "user",
            MessageMeta::Assistant { .. } => "assistant",
            MessageMeta::Thinking { .. } => "thinking",
            MessageMeta::Tool { .. } => "tool",
            MessageMeta::SubAgentReply { .. } => "subAgentReply",
            MessageMeta::SubAgentFailure { .. } => "subAgentFailure",
            MessageMeta::Summary { .. } => "summary",
        }
    }
}
