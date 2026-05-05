//! Workflow-session tier (tier-3) payload definitions.
//!
//! Sits on top of `lutin-protocol::Frame`. A session listener accepts
//! `Request`, replies with `Response`, and broadcasts `Event` to every
//! authenticated subscriber of that session.
//!
//! Sessions are workflow runs, not chats. The protocol is event-stream
//! centric: workflows emit `Event::Output` items as they execute, and
//! may issue `Event::InputRequested` if they want a value from the user
//! (replied to via `Request::ProvideInput`). Workflows that are
//! non-interactive simply never request input.

pub use lutin_ids::{SessionId, Slug, WorkflowId};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Opaque id for an outstanding input request. Workflow runtime mints
/// it; client echoes it back in `ProvideInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InputId(pub u64);

/// Generic envelope for whatever a workflow emits. Variants stay
/// minimal here; richer workflow-specific shapes ride inside the
/// `Custom` payload as opaque postcard bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkflowOutput {
    /// Streamed assistant text delta (agent-loop workflows).
    AssistantText(String),
    /// Reasoning / thinking text (agent-loop workflows).
    AssistantReasoning(String),
    /// A tool call started by the workflow runtime.
    ToolCallStarted { id: String, name: String },
    /// A tool call completed; `summary` is a runtime-defined human
    /// summary, full result rides via the typed Custom path if the
    /// workflow needs structured access.
    ToolCallCompleted {
        id: String,
        ok: bool,
        summary: String,
    },
    /// Workflow-specific structured payload.
    Custom { kind: String, body: Vec<u8> },
}

/// Why a session ended. `Cancelled` reflects an explicit `Cancel`
/// request; `Completed` is the workflow's own terminal state;
/// `Failed` carries a runtime-provided message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishReason {
    Completed,
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Request {
    /// Replay buffered events from session start, then continue
    /// streaming live. Lets a late-joining client see full transcript.
    Subscribe,
    /// Best-effort cancellation of the in-flight workflow run.
    Cancel,
    /// Reply to an outstanding `InputRequested` event.
    ProvideInput { id: InputId, value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Response {
    Ok(ResponseOk),
    Err(ApiError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResponseOk {
    /// Subscribe accepted; replay events follow as broadcasts.
    Subscribed,
    Cancelled,
    InputAccepted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
pub enum ApiError {
    #[error("unknown input id")]
    UnknownInputId,
    #[error("session has already finished")]
    Finished,
    #[error("session runtime stopped")]
    RuntimeStopped,
    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    /// Workflow emitted an output item.
    Output(WorkflowOutput),
    /// Workflow is awaiting `ProvideInput` for `id`. `prompt` is a
    /// human-facing label; `schema` hints at the expected shape
    /// (free-form for now — workflows define it).
    InputRequested {
        id: InputId,
        prompt: String,
        schema: Option<String>,
    },
    /// Terminal event. No further outputs follow.
    Finished(FinishReason),
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
        let r = Request::ProvideInput {
            id: InputId(7),
            value: "hi".into(),
        };
        assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn response_roundtrip() {
        let r = Response::Err(ApiError::UnknownInputId);
        assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn event_roundtrip() {
        let e = Event::Output(WorkflowOutput::AssistantText("hello".into()));
        assert_eq!(decode::<Event>(&encode(&e).unwrap()).unwrap(), e);
        let e = Event::Finished(FinishReason::Completed);
        assert_eq!(decode::<Event>(&encode(&e).unwrap()).unwrap(), e);
    }
}
