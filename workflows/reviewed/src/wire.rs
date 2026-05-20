//! Wire types for the reviewed workflow. Postcard-encoded on the
//! WebSocket. This is the source of truth for any future TS protocol
//! mirror; variant order is positional in postcard.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::Verdict;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct TurnId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionState {
    pub persona: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaInfo {
    pub name: String,
    pub display_name: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewVerdict {
    Pass,
    Fix { feedback: String },
    Rethink { feedback: String },
}

impl From<&Verdict> for ReviewVerdict {
    fn from(v: &Verdict) -> Self {
        match v {
            Verdict::Pass => ReviewVerdict::Pass,
            Verdict::Fix(f) => ReviewVerdict::Fix { feedback: f.clone() },
            Verdict::Rethink(f) => ReviewVerdict::Rethink { feedback: f.clone() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Turn {
    User { id: String, text: String },
    Assistant { id: String, text: String },
    ToolCall { id: String, tool: String, args: String, output: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    Completed,
    Cancelled,
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatRequest {
    Subscribe,
    SendMessage { text: String },
    Cancel,
    SetPersona { name: Option<String> },
    ListPersonas,
    GetState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatOk {
    Subscribed { state: SessionState, turns: Vec<Turn> },
    MessageQueued { turn_id: TurnId },
    Cancelled,
    State { state: SessionState },
    StateUpdated { state: SessionState },
    Personas { personas: Vec<PersonaInfo> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum ChatError {
    #[error("internal: {message}")]
    Internal { message: String },
    #[error("turn already in flight")]
    TurnInFlight,
    #[error("no turn in flight")]
    NoTurnInFlight,
    #[error("persona not found: {name}")]
    PersonaNotFound { name: String },
}

pub type ChatResponse = Result<ChatOk, ChatError>;

/// All step-scoped events use a u64 step counter — each approved tool
/// call is one "step". Drafts that get fixed/rethought share the step
/// id of the final approved call (or, for an abandoned slot, the id
/// the next attempt will use). The whole transcript-rewind story is
/// kept *out* of the wire: the UI sees only the final history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatEvent {
    UserMessageAppended { id: String, text: String },
    AssistantMessage { id: String, text: String },
    /// A tool call was drafted and is being reviewed. `attempt` counts
    /// retries within the same step (0 = first try).
    ToolCallDrafted {
        step_id: u64,
        attempt: u32,
        tool: String,
        args: String,
    },
    PrincipleEvaluated {
        step_id: u64,
        attempt: u32,
        principle: String,
        verdict: ReviewVerdict,
    },
    /// All principles passed. The reviewed call ran; `output` is its
    /// raw result.
    ToolCallExecuted {
        step_id: u64,
        tool: String,
        args: String,
        output: String,
    },
    StateChanged { state: SessionState },
    TurnFinished { turn_id: TurnId, reason: FinishReason },
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    postcard::to_allocvec(value).map_err(Into::into)
}

pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    postcard::from_bytes(bytes).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_send_message() {
        let r = ChatRequest::SendMessage { text: "hi".into() };
        assert_eq!(decode::<ChatRequest>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn roundtrip_verdict_variants() {
        for v in [
            ReviewVerdict::Pass,
            ReviewVerdict::Fix { feedback: "f".into() },
            ReviewVerdict::Rethink { feedback: "r".into() },
        ] {
            assert_eq!(decode::<ReviewVerdict>(&encode(&v).unwrap()).unwrap(), v);
        }
    }
}
