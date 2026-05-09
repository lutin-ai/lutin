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
    /// Provider opened a tool-call block. Fired as soon as the LLM
    /// stream announces the call (id + name); arguments are still
    /// streaming and will arrive via [`AgentEvent::ToolCallArgsDelta`].
    /// Hosts can render an in-progress placeholder against `id`.
    ToolCallStreaming {
        id: lutin_llm::CallId,
        name: lutin_llm::ToolName,
    },
    /// Incremental fragment of the tool call's `arguments` JSON, in
    /// stream order. Concatenating the fragments for a given `id`
    /// yields the raw JSON the model emitted; the agent SDK parses it
    /// internally and emits [`AgentEvent::ToolCallArgsParsed`] when
    /// the stream end is reached.
    ToolCallArgsDelta {
        id: lutin_llm::CallId,
        args: String,
    },
    /// All argument fragments for `call.id` are in and parsed; the
    /// agent is about to dispatch the tool. This is the moment hosts
    /// should start a duration timer.
    ToolCallArgsParsed(Arc<lutin_llm::ToolCall>),
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
