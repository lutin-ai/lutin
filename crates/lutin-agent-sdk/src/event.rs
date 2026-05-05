use std::sync::Arc;

use crate::{error::AgentError, loop_control::FinishReason, tools::ToolResult};

/// Streaming event from an agent run; errors shared via `Arc` so type info is preserved.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    RoundStarted {
        round: u32,
    },
    AssistantText(String),
    AssistantReasoning(String),
    AssistantMessage(lutin_llm::Message),
    ToolCallStarted(Arc<lutin_llm::ToolCall>),
    ToolCallCompleted {
        call: Arc<lutin_llm::ToolCall>,
        outcome: ToolResult,
    },
    Usage(lutin_llm::Usage),
    RoundEnded {
        round: u32,
        had_tool_calls: bool,
    },
    Finished(FinishReason),
    Error(Arc<AgentError>),
}
