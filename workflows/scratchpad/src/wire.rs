//! Wire types for the scratchpad workflow. Postcard-encoded on the
//! WebSocket; mirrored by `packages/scratchpad-protocol` on the TS side.
//!
//! Source of truth: this module. The UI types and TS codec must follow
//! variant order here exactly — postcard discriminants are positional.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::FixEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct TurnId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct StepId(pub u64);

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

/// Plan as seen on the wire. `args` is a JSON string so the TS decoder
/// can `JSON.parse` once at the boundary — postcard would otherwise have
/// to encode `serde_json::Value`'s enum shape, which is awkward to
/// mirror in TypeScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePlan {
    pub tool: String,
    pub goal: String,
    pub why_this_tool: String,
    pub considerations: Vec<String>,
    pub args: String,
}

impl WirePlan {
    pub fn from_plan(p: &crate::types::Plan) -> Self {
        Self {
            tool: p.tool.clone(),
            goal: p.goal.clone(),
            why_this_tool: "empty".to_string(),
            considerations: vec![],
            // why_this_tool: p.why_this_tool.clone(),
            // considerations: p.considerations.clone(),
            args: serde_json::to_string(&p.args).unwrap_or_else(|_| "null".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireVerdict {
    Pass,
    Fix { feedback: String },
    Rethink { feedback: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipleVerdict {
    pub principle: String,
    pub verdict: WireVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireIteration {
    pub index: u32,
    pub args: String,
    pub principle_verdicts: Vec<PrincipleVerdict>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Plan,
    Iterate {
        plan: WirePlan,
        fix_log: Vec<FixEntry>,
        current_principle: Option<String>,
    },
    Execute {
        plan: WirePlan,
    },
    Summarize {
        plan: WirePlan,
        output: String,
    },
    Done {
        plan: WirePlan,
        summary: String,
        output: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistentFailure {
    pub principle: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStep {
    pub id: StepId,
    pub status: StepStatus,
    pub iterations: Vec<WireIteration>,
    pub persistent_failure: Option<PersistentFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Turn {
    User { id: String, text: String },
    Step(WireStep),
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
    Subscribed {
        state: SessionState,
        turns: Vec<Turn>,
    },
    MessageQueued {
        turn_id: TurnId,
    },
    Cancelled,
    State {
        state: SessionState,
    },
    StateUpdated {
        state: SessionState,
    },
    Personas {
        personas: Vec<PersonaInfo>,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatEvent {
    UserMessageAppended {
        id: String,
        text: String,
    },
    AssistantMessage {
        id: String,
        text: String,
    },
    StepStarted {
        step_id: StepId,
    },
    PlanProposed {
        step_id: StepId,
        plan: WirePlan,
    },
    PlanRethink {
        step_id: StepId,
        feedback: String,
    },
    IterationStarted {
        step_id: StepId,
        index: u32,
        args: String,
    },
    PrincipleEvaluated {
        step_id: StepId,
        iteration: u32,
        principle: String,
        verdict: WireVerdict,
    },
    ScratchpadEdited {
        step_id: StepId,
        args: String,
    },
    FixLogUpdated {
        step_id: StepId,
        fix_log: Vec<FixEntry>,
    },
    CurrentPrincipleChanged {
        step_id: StepId,
        principle: Option<String>,
    },
    ExecuteStarted {
        step_id: StepId,
        plan: WirePlan,
    },
    ExecuteCompleted {
        step_id: StepId,
        output: String,
    },
    SummarizeCompleted {
        step_id: StepId,
        summary: String,
    },
    StepCompleted {
        step_id: StepId,
    },
    PersistentMustHaveFailure {
        step_id: StepId,
        principle: String,
        attempts: u32,
    },
    StateChanged {
        state: SessionState,
    },
    TurnFinished {
        turn_id: TurnId,
        reason: FinishReason,
    },
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
    fn roundtrip_plan_proposed_event() {
        let e = ChatEvent::PlanProposed {
            step_id: StepId(3),
            plan: WirePlan {
                tool: "Edit".into(),
                goal: "g".into(),
                why_this_tool: "w".into(),
                considerations: vec!["c1".into(), "c2".into()],
                args: "{\"path\":\"x\"}".into(),
            },
        };
        assert_eq!(decode::<ChatEvent>(&encode(&e).unwrap()).unwrap(), e);
    }

    #[test]
    fn roundtrip_verdict_variants() {
        for v in [
            WireVerdict::Pass,
            WireVerdict::Fix {
                feedback: "f".into(),
            },
            WireVerdict::Rethink {
                feedback: "r".into(),
            },
        ] {
            assert_eq!(decode::<WireVerdict>(&encode(&v).unwrap()).unwrap(), v);
        }
    }

    #[test]
    fn roundtrip_response_err() {
        let e: ChatResponse = Err(ChatError::PersonaNotFound { name: "x".into() });
        assert_eq!(decode::<ChatResponse>(&encode(&e).unwrap()).unwrap(), e);
    }
}
