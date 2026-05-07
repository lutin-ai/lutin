//! Chat workflow protocol + per-session state.
//!
//! The chat workflow runs as its own subprocess (one per session)
//! spawned by CP. It does not share `lutin-session-protocol` with the
//! project tier — workflows define their own request/response shapes.
//! The wire envelope is `lutin_protocol::Frame`; payloads ride inside
//! `Frame::Payload.body` / `Frame::Broadcast.body` as postcard-encoded
//! values of the types declared here. Protocol items live at the crate
//! root so `engine.rs` can keep its existing `use chat::{ChatRequest, …}`
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
    /// List installed personas (global + project-scoped) so the UI can
    /// render a picker. Returns enough metadata to display the
    /// dropdown without a second round-trip.
    ListPersonas,
    /// Run the agent loop against the existing transcript without
    /// appending a new user message. Used by the "rerun" affordance
    /// in the chat UI when the user wants another assistant pass on
    /// what's already there.
    Rerun,
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
                    },
                    history: vec![HistoricalMessage {
                        role: HistoricalRole::User,
                        text: "hi".into(),
                    }],
                }))
                .unwrap(),
            ),
            (
                "ChatResponse::Err(NoTurnInFlight)",
                encode::<ChatResponse>(&Err(ChatError::NoTurnInFlight)).unwrap(),
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
                &[0x00, 0x00, 0x00, 0x00, 0x00],
            ),
            (
                "ChatResponse::Ok(Subscribed{persona,1msg})",
                &[
                    0x00, 0x00, // Ok, Subscribed
                    0x01, 0x05, b'a', b'l', b'i', b'c', b'e', // Some("alice")
                    0x00, // model_override None
                    0x01, // history len 1
                    0x00, // role User
                    0x02, b'h', b'i', // text "hi"
                ],
            ),
            ("ChatResponse::Err(NoTurnInFlight)", &[0x01, 0x00]),
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
        };
        save_state(tmp.path(), &s).unwrap();
        assert_eq!(load_state(tmp.path()).unwrap(), s);
    }
}
