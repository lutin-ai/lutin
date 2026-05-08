pub(crate) mod agent;
pub(crate) mod approval;
pub(crate) mod config;
pub(crate) mod error;
pub(crate) mod event;
pub(crate) mod loop_control;
pub(crate) mod outcome;
pub(crate) mod projection;
pub(crate) mod run;
pub(crate) mod sampling;
pub(crate) mod tools;

pub use agent::Agent;
pub use approval::{AllowAll, Approval, ApprovalPolicy, DenyAll};
pub use config::AgentConfig;
pub use error::{AgentBusy, AgentError};
pub use event::AgentEvent;
pub use loop_control::{
    FinishReason, LoopConfig, LoopDetection, PreRoundHook, PreRoundOutput, RecoveryPolicy,
    RoundSummary, StopCondition, ToolCallOutcome, ToolCallRecord,
};
pub use outcome::RunOutcome;
pub use projection::{slide_window_by_user_turns, MessageProjector};
pub use sampling::{PenaltyParams, ReasoningParams, SamplingParams};
pub use tools::{Tool, ToolCallContext, ToolError, ToolPolicy, ToolResult, Toolbox};

