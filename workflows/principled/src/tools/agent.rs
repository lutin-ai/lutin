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
//! Validation seam: `SpawnAgent::call` resolves the requested persona
//! against the on-disk listing before sending the `Spawn` command. The
//! loaded `Persona` then rides the spec into the registry so the
//! spawner doesn't re-resolve — one disk read per spawn, one place
//! where "no such persona" can happen.

use std::sync::Arc;

use async_trait::async_trait;
use lutin_entities::Persona;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter, ToolResultContent};
use lutin_storage::Resolver;
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
    resolver: Arc<Resolver>,
    // `Some(owner_id)` when this tool set is built for a sub-agent (so
    // any further spawn it does is tagged as that sub-agent's child);
    // `None` for the main session's orchestrator (top-level spawns).
    owner_id: Option<AgentId>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(SpawnAgent {
            cmd_tx: cmd_tx.clone(),
            resolver,
            owner_id,
        }),
        Box::new(GetAgent { cmd_tx: cmd_tx.clone() }),
        Box::new(StopAgent { cmd_tx }),
    ]
}

const TOOL_SPAWN: &str = "spawn_agent";
const TOOL_GET: &str = "get_agent";
const TOOL_STOP: &str = "stop_agent";

pub struct SpawnAgent {
    cmd_tx: mpsc::UnboundedSender<AgentRegistryCmd>,
    /// Shared resolver so the tool can load the requested persona at
    /// the boundary. The loaded `Persona` then ships through the spec
    /// to the spawner — no second disk read on the way to `Agent`
    /// construction. An `Arc` so every `make_subagent_tools` call
    /// (one per sub-agent build) reuses the same handle.
    resolver: Arc<Resolver>,
    owner_id: Option<AgentId>,
}

#[derive(Deserialize)]
struct SpawnInput {
    initial_prompt: String,
    persona: String,
}

#[async_trait]
impl Tool for SpawnAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(TOOL_SPAWN),
            description: "Spawn a sub-agent to handle a delegated task. Returns the agent id (e.g. \"agent#7\") immediately; the child runs in the background and its final response is auto-injected into this conversation when it completes. Do NOT poll get_agent waiting for it — finish your turn after spawning; you will be re-invoked with the child's result.".into(),
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
                    description: "Persona that sub-agent will use.".into(),
                    required: true,
                },
            ],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let ToolCall { id: call_id, arguments, .. } = call;
        let (content, is_error) = match self.do_spawn(arguments).await {
            Ok(msg) => (msg, false),
            Err(msg) => (msg, true),
        };
        ToolResult::Ok(ToolResultContent { call_id, content, is_error })
    }
}

impl SpawnAgent {
    /// Validate input → load persona → ship spec → await registry id.
    /// Each step short-circuits with a tool-facing error string; the
    /// caller maps `Result` to the LLM tool's `(content, is_error)`
    /// pair. Only `EntityError::NotFound` is converted to a tool error
    /// — every other persona load failure is an operator bug (corrupt
    /// TOML, IO failure) that the LLM can't recover from, so it
    /// surfaces as a hard error too but with the underlying message.
    async fn do_spawn(&self, arguments: serde_json::Value) -> Result<String, String> {
        let SpawnInput { initial_prompt, persona } =
            serde_json::from_value(arguments).map_err(|e| format!("invalid input: {e}"))?;
        let loaded = Persona::load(&self.resolver, &persona).map_err(|e| match e {
            lutin_entities::EntityError::NotFound { name, .. } => {
                format!("persona not found: {name}")
            }
            other => format!("load persona {persona:?}: {other}"),
        })?;
        let spec = AgentSpec {
            initial_prompt,
            persona: loaded,
            parent_id: self.owner_id,
        };
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AgentRegistryCmd::Spawn { spec, reply: tx })
            .map_err(|_| "sub-agent registry is unavailable".to_owned())?;
        let id = rx
            .await
            .map_err(|_| "sub-agent registry dropped the request".to_owned())?;
        Ok(format!("spawned {id}"))
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
            description: "Read the current status of a previously-spawned sub-agent. Returns running / completed / failed{reason} / stopped. Use this only when the user explicitly asks about a child's status — do NOT call it to poll for completion. The same status is auto-injected into the <active_subagents> system-prompt block each turn, and a child's final response is auto-injected into the conversation when it completes; polling is never necessary and wastes rounds.".into(),
            parameters: vec![ToolParameter {
                name: "id".into(),
                r#type: "string".into(),
                description: "Agent id as returned by spawn_agent (accepts either \"agent#N\" or the bare number).".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let (content, is_error) = match self.do_get(&call.arguments).await {
            Ok(msg) => (msg, false),
            Err(msg) => (msg, true),
        };
        ToolResult::Ok(ToolResultContent { call_id: call.id, content, is_error })
    }
}

impl GetAgent {
    async fn do_get(&self, arguments: &serde_json::Value) -> Result<String, String> {
        let id = parse_id(arguments)?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AgentRegistryCmd::Status { id, reply: tx })
            .map_err(|_| "sub-agent registry is unavailable".to_owned())?;
        match rx.await {
            Ok(Some(status)) => Ok(render_status(id, &status)),
            Ok(None) => Err(format!("no such agent: {id}")),
            Err(_) => Err("sub-agent registry dropped the request".to_owned()),
        }
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
        let (content, is_error) = match self.do_stop(&call.arguments).await {
            Ok(msg) => (msg, false),
            Err(msg) => (msg, true),
        };
        ToolResult::Ok(ToolResultContent { call_id: call.id, content, is_error })
    }
}

impl StopAgent {
    async fn do_stop(&self, arguments: &serde_json::Value) -> Result<String, String> {
        let id = parse_id(arguments)?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AgentRegistryCmd::Stop { id, reply: tx })
            .map_err(|_| "sub-agent registry is unavailable".to_owned())?;
        rx.await
            .map_err(|_| "sub-agent registry dropped the request".to_owned())?;
        Ok(format!("stopped {id}"))
    }
}

/// Accept the id either as a bare integer (`{"id": 7}`) or as a string
/// (`{"id": "agent#7"}` or `{"id": "7"}`). Delegates to
/// `AgentId::from_str` for the string case so the wire-format rule
/// lives on the type itself, not duplicated across handlers.
fn parse_id(arguments: &serde_json::Value) -> Result<AgentId, String> {
    let raw = arguments
        .get("id")
        .ok_or_else(|| "missing field `id`".to_string())?;
    if let Some(n) = raw.as_u64() {
        return Ok(AgentId(n));
    }
    if let Some(s) = raw.as_str() {
        return s.parse::<AgentId>().map_err(|e| e.to_string());
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
