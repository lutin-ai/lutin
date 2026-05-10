//! Agent-self-issued rewind. The model calls `abort_step` when it
//! decides it can't reconcile the active step's reviewer feedback —
//! e.g. when the iteration gate is blocking a tool it actually needs,
//! or when the step's premise is wrong and a different earlier step
//! should be reconsidered.
//!
//! The tool sends a [`RewindSignal::Continue`] carrying the agent's
//! `reason` as feedback. The runner's existing `rewind_rx` arm picks
//! it up, cancels the agent, restores the prior frame's snapshot, and
//! injects the `reason` (combined with any reviewer feedback) into
//! the reactivated frame's `carried_forward` — so the next attempt
//! has the agent's own self-assessment in context.

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter, ToolResultContent};
use lutin_tools::{Tool, ToolCallContext, ToolResult};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::review::{RewindSignal, ABORT_STEP_TOOL_NAME, SELF_ABORT_FEEDBACK_PREFIX};

/// Build the tool. `rewind_tx` is the same channel the reviewers use
/// for `Rethink` verdicts — one consumer (the runner), so cloning the
/// sender here is safe and keeps the rewind path single-armed.
pub fn make_abort_step_tool(
    rewind_tx: mpsc::UnboundedSender<RewindSignal>,
) -> Box<dyn Tool> {
    Box::new(AbortStep { rewind_tx })
}

struct AbortStep {
    rewind_tx: mpsc::UnboundedSender<RewindSignal>,
}

#[derive(Deserialize)]
struct AbortInput {
    reason: String,
}

#[async_trait]
impl Tool for AbortStep {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(ABORT_STEP_TOOL_NAME),
            description: "Rewind the current review iteration. Call this when you cannot satisfy the reviewers with the locked tool — e.g. you need a different tool, or you've realized an earlier decision was wrong. The step's file effects are reverted, the agent rewinds to the prior step (if any), and your `reason` is carried forward into the next attempt as context. Use this instead of fighting the iteration gate.".into(),
            parameters: vec![ToolParameter {
                name: "reason".into(),
                r#type: "string".into(),
                description: "What you've realized that justifies rewinding. Be specific — this text is the only signal the next attempt has about why you backed out.".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let ToolCall { id: call_id, arguments, .. } = call;
        let (content, is_error) = match serde_json::from_value::<AbortInput>(arguments) {
            Ok(AbortInput { reason }) => {
                let feedback = format!("{SELF_ABORT_FEEDBACK_PREFIX}{reason}");
                if self
                    .rewind_tx
                    .send(RewindSignal::Continue { feedback })
                    .is_err()
                {
                    (
                        "rewind channel unavailable — runner is gone".to_string(),
                        true,
                    )
                } else {
                    (
                        format!("rewinding step. carried forward: {reason}"),
                        false,
                    )
                }
            }
            Err(e) => (format!("invalid input: {e}"), true),
        };
        ToolResult::Ok(ToolResultContent { call_id, content, is_error })
    }
}
