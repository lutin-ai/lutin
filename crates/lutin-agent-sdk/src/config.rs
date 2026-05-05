use std::sync::Arc;

use lutin_llm::{LlmProvider, ModelId};

use crate::{loop_control::LoopConfig, sampling::SamplingParams, tools::ToolPolicy};

/// Pairs the LLM provider with the model: each model can ride its own
/// provider, and switching model implies switching the backend that knows
/// how to serve it.
#[derive(Clone)]
pub struct AgentConfig {
    pub provider: Arc<dyn LlmProvider>,
    pub model: ModelId,
    pub sampling: SamplingParams,
    pub system: String,
    pub tool_policy: ToolPolicy,
    pub loop_config: LoopConfig,
}
