use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

/// Outcome class for a completed tool call. Distinguishes successful dispatch
/// from any failure mode (denied by approval, errored, timed out, invalid args)
/// without conflating them into a single boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolCallOutcome {
    /// Dispatch completed with `ToolResult::Ok`.
    Ok,
    /// Denied, errored, timed-out, or invalid-args — anything non-Ok.
    Failed,
}

/// Record of a completed tool call within a round; passed to custom stop predicates
/// so they can decide based on which tools fired. Arguments are intentionally
/// omitted — hosts that need them can snapshot from `AgentEvent::ToolCallStarted`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ToolCallRecord {
    pub name: lutin_llm::ToolName,
    pub id: lutin_llm::CallId,
    pub outcome: ToolCallOutcome,
}

impl ToolCallRecord {
    pub fn new(
        name: lutin_llm::ToolName,
        id: lutin_llm::CallId,
        outcome: ToolCallOutcome,
    ) -> Self {
        Self { name, id, outcome }
    }
}

/// Output of the per-round hook: optional user-side message injection plus an
/// optional refreshed system prompt that applies to the upcoming request only.
#[derive(Default)]
#[non_exhaustive]
pub struct PreRoundOutput {
    pub inject_messages: Vec<lutin_llm::Message>,
    /// When `Some`, the driver replaces the system prompt for the upcoming
    /// round's `CompletionRequest`. The agent's stored system string is not
    /// mutated — the fresh value persists only for this round.
    pub system: Option<String>,
}

impl PreRoundOutput {
    /// Common case: only inject messages, no system refresh.
    pub fn with_messages(inject_messages: Vec<lutin_llm::Message>) -> Self {
        Self { inject_messages, system: None }
    }
}

/// Async callback invoked before each round's LLM request is built.
/// Returns messages to append to the transcript and an optional fresh system
/// prompt for the upcoming round.
pub type PreRoundHook =
    Arc<dyn Fn(u32) -> BoxFuture<'static, PreRoundOutput> + Send + Sync>;

/// Predicate signature used by [`StopCondition::Custom`]. Factored into a type
/// alias because the inline form trips `clippy::type_complexity`.
pub type CustomStopFn =
    Arc<dyn Fn(&RoundSummary, &[ToolCallRecord]) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct LoopConfig {
    pub max_rounds: u32,
    pub stop_condition: StopCondition,
    pub loop_detection: LoopDetection,
    pub recovery: RecoveryPolicy,
    /// Per-round hook: called with the round number immediately before the
    /// LLM request is built. Returned messages are appended to the transcript.
    pub pre_round: Option<PreRoundHook>,
    /// Maximum time to wait between stream events before aborting the round
    /// with `AgentError::StreamStalled`. `None` = no inactivity limit.
    pub stream_inactivity_timeout: Option<Duration>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_rounds: 32,
            stop_condition: StopCondition::NoToolCalls,
            loop_detection: LoopDetection::Disabled,
            recovery: RecoveryPolicy::FailFast,
            pre_round: None,
            stream_inactivity_timeout: None,
        }
    }
}

#[derive(Clone)]
#[non_exhaustive]
pub enum StopCondition {
    NoToolCalls,
    MaxRounds,
    /// Stop once any completed tool call in a round had the given name.
    ToolCalled(lutin_llm::ToolName),
    /// Stop as soon as a round contains at least one denied tool call.
    AnyCallDenied,
    Custom(CustomStopFn),
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RoundSummary {
    pub round: u32,
    pub had_tool_calls: bool,
    pub assistant_text_len: usize,
    pub tool_call_count: u32,
    /// Number of tool calls denied by the approval policy during this round.
    pub denied_count: u32,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LoopDetection {
    Disabled,
    SameToolCallRepeated { threshold: u32 },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RecoveryPolicy {
    FailFast,
    RetryTransient { max_attempts: u32 },
}

/// Terminal reason a run ended; exactly one per `Finished` event.
///
/// The `Error` variant carries the `AgentError` that caused termination so
/// "errored" and "has-an-error" can never disagree at the type level.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FinishReason {
    Stopped,
    MaxRounds,
    LoopDetected,
    Cancelled,
    Error(std::sync::Arc<crate::error::AgentError>),
}
