use thiserror::Error;

/// Returned by mutators / `Agent::start` when agent is running.
#[derive(Debug, Error)]
#[error("agent busy: cannot mutate while running")]
pub struct AgentBusy;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentError {
    #[error("provider error: {0}")]
    Provider(#[from] lutin_llm::LlmError),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("loop detected: {0}")]
    LoopDetected(String),

    #[error("max rounds reached ({0})")]
    MaxRounds(u32),

    #[error("cancelled")]
    Cancelled,

    #[error("stream stalled: no events for {0:?}")]
    StreamStalled(std::time::Duration),

    #[error("internal: {0}")]
    Internal(String),
}
