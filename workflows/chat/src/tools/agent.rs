//! LLM-facing tools for spawning, inspecting, and stopping sub-agents.
//!
//! Each tool holds a clone of the registry's
//! [`mpsc::UnboundedSender<AgentRegistryCmd>`](crate::agents::AgentRegistryCmd)
//! and replies to the LLM via a `oneshot` round-trip — same actor model
//! used everywhere else in this workflow, no shared mutable state.
//!
//! Gating: the persona's `tool_filter_list` decides whether these tools
//! are visible to a given run. The orchestrator persona whitelists
//! `spawn_agent` / `get_agent` / `stop_agent`; other personas don't.
//! See the comment on [`make_subagent_tools`] for why we always inject
//! all three (the filter does the gating, not the call site).
//!
//! Wire choice (v1): [`crate::agents::AgentSpec::transcript_snapshot`]
//! is set to an empty `Arc<Vec<_>>` — sub-agents start with the
//! delegator's brief alone, not the parent's transcript. The
//! orchestrator persona's prompt instructs it to pack purpose +
//! acceptance criteria into the brief, which is the canonical input.
//! Plumbing the live transcript through every tool call would mean
//! either threading the parent `Agent`'s message vec into tool dispatch
//! or re-loading it from disk on every spawn — both pay a real cost
//! for context the orchestrator isn't supposed to lean on. Revisit if
//! a concrete need shows up (a "fork this conversation" tool).

use std::sync::Arc;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter, ToolResultContent};
use lutin_tools::{Tool, ToolCallContext, ToolResult};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::agents::{AgentId, AgentRegistryCmd, AgentSpec, AgentStatus};

/// Build the three sub-agent tools. Every persona that passes the
/// chat workflow's filter still funnels through
/// `lutin_tools::filter_by_name`; non-orchestrator personas drop these
/// by name without the engine needing to know which personas count as
/// orchestrators. Returning `Vec<Box<dyn Tool>>` keeps the seam at the
/// SDK boundary the same as `default_tools`.
pub fn make_subagent_tools(
    cmd_tx: mpsc::UnboundedSender<AgentRegistryCmd>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(SpawnAgent { cmd_tx: cmd_tx.clone() }),
        Box::new(GetAgent { cmd_tx: cmd_tx.clone() }),
        Box::new(StopAgent { cmd_tx }),
    ]
}

const TOOL_SPAWN: &str = "spawn_agent";
const TOOL_GET: &str = "get_agent";
const TOOL_STOP: &str = "stop_agent";

pub struct SpawnAgent {
    cmd_tx: mpsc::UnboundedSender<AgentRegistryCmd>,
}

#[derive(Deserialize)]
struct SpawnInput {
    initial_prompt: String,
    #[serde(default)]
    persona: Option<String>,
}

#[async_trait]
impl Tool for SpawnAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(TOOL_SPAWN),
            description: "Spawn a sub-agent to handle a delegated task. Returns the agent id (e.g. \"agent#7\") immediately; the child runs in the background and its final response is auto-injected into this conversation when it completes. Include purpose + acceptance criteria in `initial_prompt` — the child does not see the parent's transcript.".into(),
            parameters: vec![
                ToolParameter {
                    name: "initial_prompt".into(),
                    r#type: "string".into(),
                    description: "The full brief for the sub-agent: task, purpose, acceptance criteria, scope limits.".into(),
                    required: true,
                },
                ToolParameter {
                    name: "persona".into(),
                    r#type: "string".into(),
                    description: "Optional persona name to instantiate. When omitted, the child inherits this session's current persona.".into(),
                    required: false,
                },
            ],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let ToolCall { id: call_id, arguments, .. } = call;
        let (content, is_error) = match serde_json::from_value::<SpawnInput>(arguments) {
            Ok(SpawnInput { initial_prompt, persona }) => {
                let spec = AgentSpec {
                    initial_prompt,
                    persona,
                    transcript_snapshot: Arc::new(Vec::new()),
                };
                let (tx, rx) = oneshot::channel();
                if self.cmd_tx.send(AgentRegistryCmd::Spawn { spec, reply: tx }).is_err() {
                    err("sub-agent registry is unavailable")
                } else {
                    match rx.await {
                        Ok(id) => ok(format!("spawned {id}")),
                        Err(_) => err("sub-agent registry dropped the request"),
                    }
                }
            }
            Err(e) => err(format!("invalid input: {e}")),
        };
        ToolResult::Ok(ToolResultContent { call_id, content, is_error })
    }
}

pub struct GetAgent {
    cmd_tx: mpsc::UnboundedSender<AgentRegistryCmd>,
}

#[async_trait]
impl Tool for GetAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(TOOL_GET),
            description: "Read the current status of a previously-spawned sub-agent. Returns running / completed / failed{reason} / stopped. The same status is also visible in the <active_subagents> system-prompt block.".into(),
            parameters: vec![ToolParameter {
                name: "id".into(),
                r#type: "string".into(),
                description: "Agent id as returned by spawn_agent (accepts either \"agent#N\" or the bare number).".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let (content, is_error) = match parse_id(&call.arguments) {
            Ok(id) => {
                let (tx, rx) = oneshot::channel();
                if self.cmd_tx.send(AgentRegistryCmd::Status { id, reply: tx }).is_err() {
                    err("sub-agent registry is unavailable")
                } else {
                    match rx.await {
                        Ok(Some(status)) => ok(render_status(id, &status)),
                        Ok(None) => err(format!("no such agent: {id}")),
                        Err(_) => err("sub-agent registry dropped the request"),
                    }
                }
            }
            Err(reason) => err(reason),
        };
        ToolResult::Ok(ToolResultContent { call_id: call.id, content, is_error })
    }
}

pub struct StopAgent {
    cmd_tx: mpsc::UnboundedSender<AgentRegistryCmd>,
}

#[async_trait]
impl Tool for StopAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(TOOL_STOP),
            description: "Cancel a running sub-agent. The child is aborted; no response message is auto-injected. A no-op against an already-terminal agent.".into(),
            parameters: vec![ToolParameter {
                name: "id".into(),
                r#type: "string".into(),
                description: "Agent id as returned by spawn_agent (accepts either \"agent#N\" or the bare number).".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let (content, is_error) = match parse_id(&call.arguments) {
            Ok(id) => {
                let (tx, rx) = oneshot::channel();
                if self.cmd_tx.send(AgentRegistryCmd::Stop { id, reply: tx }).is_err() {
                    err("sub-agent registry is unavailable")
                } else {
                    match rx.await {
                        Ok(()) => ok(format!("stopped {id}")),
                        Err(_) => err("sub-agent registry dropped the request"),
                    }
                }
            }
            Err(reason) => err(reason),
        };
        ToolResult::Ok(ToolResultContent { call_id: call.id, content, is_error })
    }
}

/// Accept the id either as a bare integer (`{"id": 7}`) or as the
/// canonical display form (`{"id": "agent#7"}`). Anything else is a
/// 400-style error back to the LLM so it can correct itself rather
/// than silently spawning at id 0 or similar.
fn parse_id(arguments: &serde_json::Value) -> Result<AgentId, String> {
    let raw = arguments
        .get("id")
        .ok_or_else(|| "missing field `id`".to_string())?;
    if let Some(n) = raw.as_u64() {
        return Ok(AgentId(n));
    }
    if let Some(s) = raw.as_str() {
        let stripped = s.strip_prefix("agent#").unwrap_or(s);
        return stripped
            .parse::<u64>()
            .map(AgentId)
            .map_err(|_| format!("unparseable agent id: {s:?}"));
    }
    Err(format!("agent id must be string or integer, got {raw}"))
}

fn render_status(id: AgentId, status: &AgentStatus) -> String {
    match status {
        AgentStatus::Running => format!("{id} status=running"),
        AgentStatus::Completed => format!("{id} status=completed"),
        AgentStatus::Failed { reason } => format!("{id} status=failed reason={reason:?}"),
        AgentStatus::Stopped => format!("{id} status=stopped"),
    }
}

fn ok(s: impl Into<String>) -> (String, bool) {
    (s.into(), false)
}

fn err(s: impl Into<String>) -> (String, bool) {
    (s.into(), true)
}
