//! Portable tool implementations.
//!
//! Each tool is a zero-wrapper struct implementing the local [`Tool`] trait.
//! Compose several with [`multi::Toolbox`] to register them on one agent.
//! The context carried by every tool is [`context::ToolContext`] — a small,
//! engine-agnostic bundle of sandbox root, env vars, HTTP client, and
//! read-state tracker.

use async_trait::async_trait;
use thiserror::Error;

pub mod builders;
pub mod context;
pub mod multi;
mod outcome;
pub mod read_state;

pub mod file_edit;
pub mod file_edit_lines;
pub mod file_glob;
pub mod file_grep;
pub mod file_list;
pub mod file_read;
pub mod file_tree;
pub mod file_write;
pub mod http_request;
pub mod image_view;
pub mod shell;
pub mod wait;
pub mod web_search;

pub use builders::{default_tools, filter_by_name, FilterMode};
pub use context::ToolContext;
pub use multi::{BuildError, Toolbox};
pub use read_state::ReadState;

/// Minimal error type for tool dispatch failures.
#[derive(Debug, Error, Clone)]
#[non_exhaustive]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("execution failed: {0}")]
    Execution(String),
    // Why: surfaced by agent SDK when a per-call timeout or approval gate
    // fires before/around dispatch. Kept here so the ToolResult type covers
    // both intrinsic-tool and dispatch-layer failure modes.
    #[error("timeout")]
    Timeout,
    #[error("denied: {0}")]
    Denied(String),
}

/// Result of a tool invocation. Wraps the LLM-facing `ToolResultContent`
/// produced by a successful call, or a dispatch error.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ToolResult {
    Ok(lutin_llm::ToolResultContent),
    Err(ToolError),
}

/// Per-call context. Carries round and call-index metadata so tools can
/// reason about where in the agent's loop they're being invoked.
#[derive(Debug, Clone, Default)]
pub struct ToolCallContext {
    pub round: u32,
    pub call_index: u32,
}

/// Trait implemented once per tool. Each impl returns a single
/// [`lutin_llm::ToolDefinition`] from [`Tool::definition`].
#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> lutin_llm::ToolDefinition;
    async fn call(&self, ctx: &ToolCallContext, call: lutin_llm::ToolCall) -> ToolResult;
}
