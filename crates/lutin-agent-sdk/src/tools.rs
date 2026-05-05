//! Tool plumbing for the agent SDK.
//!
//! Tool primitives (Tool trait, Toolbox, ToolError, ToolResult, ToolCallContext)
//! now live in `lutin_tools`; we re-export them and keep SDK-local concerns
//! (`ToolPolicy`, `NoTools`) here.

use std::time::Duration;

pub use lutin_tools::{Tool, ToolCallContext, ToolError, ToolResult, Toolbox};

#[derive(Debug, Clone)]
pub struct ToolPolicy {
    pub max_calls_per_round: u32,
    pub per_call_timeout: Option<Duration>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            max_calls_per_round: 32,
            per_call_timeout: None,
        }
    }
}

/// Convenience: an empty toolbox. The agent uses this when no tools have
/// been configured.
pub(crate) struct NoTools;

impl NoTools {
    /// Build an empty `Toolbox`. Cannot fail — empty input has no duplicates.
    pub fn toolbox() -> Toolbox {
        Toolbox::new(Vec::new()).expect("empty toolbox cannot fail to build")
    }
}
