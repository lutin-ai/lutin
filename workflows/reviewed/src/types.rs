use std::path::PathBuf;
use std::sync::Arc;

use lutin_llm::{LlmProvider, Message, ModelId};
use lutin_memory::Memory;
use lutin_storage::Resolver;
use lutin_tools::Toolbox;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::wire::ChatEvent;

pub struct Agent {
    pub persona: String,
    pub provider: Arc<dyn LlmProvider>,
    pub model: ModelId,
    pub temperature: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub thinking_enabled: bool,
    pub reasoning: Option<lutin_llm::Reasoning>,
    pub messages: Vec<Message>,
    pub toolbox: Toolbox,
    pub state_dir: PathBuf,
    /// Workspace root file paths resolve against — used by the post-edit
    /// format/check probes.
    pub sandbox_root: PathBuf,
    pub resolver: Arc<Resolver>,
    pub events: broadcast::Sender<ChatEvent>,
    /// Episodic memory store for this session, if it opened. Events are
    /// recorded here as the turn runs; `None` disables memory fail-soft.
    pub memory: Option<Arc<Memory>>,
    /// External chat id that groups this session's events (the session id).
    pub chat_id: String,
    /// Per-tool pass streaks driving recency sampling of the reviewers.
    pub recency: std::sync::Mutex<crate::recency::RecencyState>,
    /// Providers built for reviewer personas whose `provider` differs from
    /// the main agent's, keyed by provider name. Rebuilt with the agent
    /// each turn, so settings edits still take effect on the next turn.
    pub reviewer_providers: std::sync::Mutex<std::collections::HashMap<String, Arc<dyn LlmProvider>>>,
}

#[derive(Debug, Clone)]
pub enum TurnOutcome {
    Yield { reply: String },
}

#[derive(Debug, Clone)]
pub enum Verdict {
    Pass,
    Fail(String),
}

/// Adapter shape passed to the reviewer. The reviewer treats this as
/// "the tool call being judged"; in this workflow it's the assistant's
/// just-drafted tool call.
#[derive(Debug, Clone)]
pub struct ReviewedCall {
    pub tool: String,
    pub goal: String,
    pub args: serde_json::Value,
}

/// The `applies_to` label a principle uses to opt into message-level
/// (non-tool) review — both the pre-text steer and the post-text gate.
pub const MESSAGE_TRIGGER: &str = "message";

/// What the reviewers are judging this round: a drafted tool call, the
/// incoming user request (pre-text), or the agent's drafted final reply
/// (post-text). The two message subjects share the `message` trigger but
/// are tracked separately by recency.
#[derive(Debug, Clone, Copy)]
pub enum ReviewSubject<'a> {
    Tool(&'a ReviewedCall),
    UserMessage(&'a str),
    AgentReply(&'a str),
}

impl ReviewSubject<'_> {
    /// Label matched against a principle's `applies_to`.
    pub fn trigger(&self) -> &str {
        match self {
            ReviewSubject::Tool(c) => &c.tool,
            ReviewSubject::UserMessage(_) | ReviewSubject::AgentReply(_) => MESSAGE_TRIGGER,
        }
    }

    /// Per-subject recency key. Pre- and post-text get distinct keys so
    /// trusting one never silences the other.
    pub fn recency_key(&self) -> &str {
        match self {
            ReviewSubject::Tool(c) => &c.tool,
            ReviewSubject::UserMessage(_) => "message:pre",
            ReviewSubject::AgentReply(_) => "message:post",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Principle {
    #[serde(skip)]
    pub name: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub kind: PrincipleKind,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default = "default_points")]
    pub points: Option<u8>,
    /// Recency sampling control. `-1` = always checked. `>= 1` = the
    /// number of consecutive per-tool passes after which this check is
    /// "trusted" and drops to periodic re-checking. Sole sampling control.
    #[serde(default = "default_recency_points")]
    pub recency_points: i32,
    pub persona: String,
    /// Evaluation order within a tree level: lower runs first, ties broken
    /// by name. The first principle to block is decisive, so this controls
    /// which objection the agent sees when several would fire.
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub applies_to: Vec<String>,
    #[serde(default)]
    pub context: Vec<ContextItem>,
    /// Investigation tools this principle's reviewer may call before its
    /// verdict, by name, resolved against the main toolbox. Empty (the
    /// default) = pure judgment — fastest, and keeps the reviewer prompt
    /// prefix shared across principles.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Natural-language relevance gate. On a node with `children` the
    /// reviewer first decides whether this condition holds; only then are
    /// the sub-principles evaluated. Empty on most leaves.
    #[serde(default)]
    pub condition: String,
    /// Sub-principles. A node with children is a GATE (its condition
    /// decides whether to descend); a leaf is a CHECK. Built from the
    /// filesystem hierarchy in `principle.rs`, not from the TOML body.
    #[serde(skip)]
    pub children: Vec<Principle>,
}

impl Principle {
    pub fn is_gate(&self) -> bool {
        !self.children.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextItem {
    ToolCall,
    ToolArtifact,
    Chat,
    PriorSteps,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrincipleKind {
    Plan,
    #[default]
    Impl,
    Review,
}

fn default_required() -> bool {
    false
}

fn default_points() -> Option<u8> {
    Some(3)
}

fn default_recency_points() -> i32 {
    -1
}

fn default_priority() -> i32 {
    100
}
