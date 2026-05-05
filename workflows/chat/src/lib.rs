//! Chat workflow protocol + per-session state, plus the `ui` submodule
//! exporting the `cdylib` UI plugin loaded by the desktop chrome.
//!
//! The chat workflow runs as its own subprocess (one per session)
//! spawned by `lutin-project`. It does not share `lutin-session-protocol`
//! with the project tier — workflows define their own request/response
//! shapes. The wire envelope is still `lutin_protocol::Frame`; payloads
//! ride inside `Frame::Payload.body` / `Frame::Broadcast.body` as
//! postcard-encoded values of the types declared here. Protocol items
//! live at the crate root so `engine.rs` can keep its existing
//! `use chat::{ChatRequest, …}` imports.

pub mod ui;

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
}

/// One entry in the rendered scrollback. The engine projects its full
/// `Vec<lutin_llm::Message>` to this UI-friendly shape on `Subscribe`,
/// dropping anything the chat UI doesn't render (tool calls, system
/// prompt, raw images). Adds bytes proportional to text length, which
/// is fine for chat sessions (~hundreds of messages max).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoricalMessage {
    pub role: HistoricalRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoricalRole {
    User,
    Assistant,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatEvent {
    /// Streaming assistant text delta.
    Delta(String),
    /// Streaming reasoning / thinking delta.
    Reasoning(String),
    ToolCallStarted { id: String, name: String },
    ToolCallCompleted { id: String, ok: bool, summary: String },
    /// Terminal event for one turn.
    MessageFinished { turn_id: TurnId, reason: FinishReason },
    /// Pushed when `SessionState` mutates so subscribers can rerender.
    StateChanged(SessionState),
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

    #[test]
    fn state_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let s = SessionState {
            persona: Some("assistant".into()),
            model_override: None,
        };
        save_state(tmp.path(), &s).unwrap();
        assert_eq!(load_state(tmp.path()).unwrap(), s);
    }
}
