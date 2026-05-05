//! Project tier (tier-2) payload definitions.
//!
//! Sits on top of `lutin-protocol::Frame`. A project supervisor
//! receives `Request`, replies with `Response`, and broadcasts `Event`
//! to every authenticated client of that project.

pub use lutin_ids::{SessionId, Slug, WorkflowId};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowInfo {
    pub id: WorkflowId,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub workflow: WorkflowId,
}

/// Where a started workflow session listens, and the token a client
/// should present when connecting directly to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEndpoint {
    pub addr: std::net::SocketAddr,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Request {
    ListWorkflows,
    ListSessions,
    StartSession { workflow: WorkflowId },
    StopSession { session: SessionId },
    OpenSession { session: SessionId },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Response {
    Ok(ResponseOk),
    Err(ApiError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResponseOk {
    Workflows(Vec<WorkflowInfo>),
    Sessions(Vec<SessionInfo>),
    Started(SessionInfo),
    Stopped,
    Opened(SessionEndpoint),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
pub enum ApiError {
    #[error("workflow not found: {0}")]
    WorkflowNotFound(WorkflowId),
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("supervisor stopped")]
    SupervisorStopped,
    #[error("supervisor dropped reply")]
    SupervisorDroppedReply,
    #[error("unauthorized")]
    Unauthorized,
    #[error("not implemented yet")]
    Unimplemented,
    /// Workflow crate failed to compile. `exit_code` is the cargo
    /// process exit code (`None` if killed by signal). Distinct from
    /// `Internal` so clients can render compile failures specifically
    /// without parsing strings.
    #[error("cargo build failed (exit_code={exit_code:?})")]
    WorkflowBuildFailed { exit_code: Option<i32> },
    #[error("internal: {0}")]
    Internal(String),
}

/// Outcome reported on the wire when a workflow build runs.
/// `Skipped` is implicit (no `WorkflowBuildStarted`/`Finished` pair
/// is emitted on the warm path) and so isn't a variant here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BuildOutcome {
    Success,
    Failed { exit_code: Option<i32> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    SessionStarted(SessionInfo),
    SessionEnded { id: SessionId },
    /// Emitted when `StartSession` triggers a cargo rebuild of the
    /// workflow crate. Skipped entirely on the warm path (binary
    /// fresh), so its absence is not an error. The `Started`/`Output`/
    /// `Finished` events all carry the same `(session, workflow)` so
    /// subscribers can route by either without out-of-band correlation.
    WorkflowBuildStarted {
        session: SessionId,
        workflow: WorkflowId,
    },
    /// One line of cargo's stdout/stderr during a workflow build.
    WorkflowBuildOutput {
        session: SessionId,
        workflow: WorkflowId,
        line: String,
    },
    /// Build terminated. A `Failed` outcome means the supervisor will
    /// also return `ApiError::WorkflowBuildFailed` to the
    /// `StartSession` caller.
    WorkflowBuildFinished {
        session: SessionId,
        workflow: WorkflowId,
        outcome: BuildOutcome,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = Request::StartSession {
            workflow: WorkflowId::parse("chat").unwrap(),
        };
        assert_eq!(decode::<Request>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn response_roundtrip() {
        let r = Response::Ok(ResponseOk::Started(SessionInfo {
            id: SessionId::parse("s1").unwrap(),
            workflow: WorkflowId::parse("chat").unwrap(),
        }));
        assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn err_response_roundtrip() {
        let r = Response::Err(ApiError::SessionNotFound(SessionId::parse("x").unwrap()));
        assert_eq!(decode::<Response>(&encode(&r).unwrap()).unwrap(), r);
    }

    #[test]
    fn event_roundtrip() {
        let e = Event::SessionEnded {
            id: SessionId::parse("s1").unwrap(),
        };
        assert_eq!(decode::<Event>(&encode(&e).unwrap()).unwrap(), e);
    }

    #[test]
    fn workflow_id_rejects_bad_char() {
        assert!(WorkflowId::parse("bad slash/").is_err());
    }
}
