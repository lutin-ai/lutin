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
/// for the cdylib bytes fetched via `GetWorkflowCdylib`.
///
/// `display_name` and `icon` come from `lutin.workflow.display_name`
/// / `lutin.workflow.icon` Docker labels and feed chrome's sidebar
/// + top-bar rendering. Chrome reads these from CP rather than from
/// the cdylib so it can decorate the chrome before the cdylib is
/// loaded (sessions trigger the dlopen).
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub workflow: WorkflowId,
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
    /// Stop a running session (terminates its container).
    StopSession {
        slug: Slug,
        session: SessionId,
    },
    /// Re-issue a token + endpoint for an already-running session.
    /// Used when the desktop reconnects to a session it had open.
    OpenSession {
        slug: Slug,
        session: SessionId,
    },
    /// Fetch the cdylib bytes for a workflow image. The desktop caches
    /// these by `digest` on its side and only requests when its cache
    /// is missing or stale relative to the digest reported by
    /// `ListWorkflows`.
    GetWorkflowCdylib {
        id: WorkflowId,
    },
    /// Fetch the static-asset bundle (tarball) for a workflow image.
    /// Replaces `GetWorkflowCdylib` post-Phase-2 — the bundle ships an
    /// HTML/JS plugin UI that runs in an iframe instead of an
    /// in-process cdylib. Same caching strategy: desktop keys by
    /// `digest` from `ListWorkflows` and only refetches on mismatch.
    GetWorkflowBundle {
        id: WorkflowId,
    },
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
    /// Reply to `OpenSession`: just an endpoint (the caller already has
    /// the `SessionInfo` from `ListSessions`).
    SessionOpened(SessionEndpoint),
    /// Reply to `GetWorkflowCdylib`. `digest` matches the image at the
    /// time of the read; desktop persists it alongside the bytes so
    /// subsequent `ListWorkflows` digest comparisons can skip refetch.
    WorkflowCdylib {
        id: WorkflowId,
        digest: String,
        bytes: Vec<u8>,
    },
    /// Reply to `GetWorkflowBundle`. `bytes` is a tar archive of the
    /// plugin UI (root-level `lutin.workflow.json` + `index.html` + any
    /// referenced assets). Desktop unpacks under its cache dir keyed
    /// by `(workflow_id, digest)`.
    WorkflowBundle {
        id: WorkflowId,
        digest: String,
        bytes: Vec<u8>,
    },
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
}
