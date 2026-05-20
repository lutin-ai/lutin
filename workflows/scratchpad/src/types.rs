use std::path::PathBuf;
use std::sync::Arc;

use lutin_llm::{CallId, LlmProvider, Message, ModelId};
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
    pub messages: Vec<Message>,
    pub toolbox: Toolbox,
    pub state: AgentState,
    pub steps: Vec<StepRecord>,
    pub state_dir: PathBuf,
    pub resolver: Arc<Resolver>,
    pub events: broadcast::Sender<ChatEvent>,
    pub summarizer_provider: Arc<dyn LlmProvider>,
    pub summarizer_model: ModelId,
    pub summarizer_system: String,
    pub summarizer_temperature: Option<f32>,
    pub summarizer_presence_penalty: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub call_id: CallId,
    pub plan: Plan,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentState {
    Plan { rethink_feedback: Option<String> },
    Iterate { plan: Plan, fix_log: Vec<FixEntry> },
    Execute { plan: Plan },
    Summarize { plan: Plan, output: String },
    Done,
    AwaitInput { reply: String },
}

#[derive(Debug, Clone)]
pub enum StepOutcome {
    /// Step finalized; more steps may follow in this turn.
    Continue,
    /// Agent yielded the turn back to the user with a final reply.
    Yield { reply: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixEntry {
    pub principle: String,
    pub feedback: String,
}

#[derive(Debug, Clone)]
pub enum Verdict {
    Pass,
    Fix(String),
    Rethink(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub tool: String,
    pub goal: String,
    #[serde(default)]
    pub considerations: Vec<String>,
    #[serde(default = "default_args")]
    pub args: serde_json::Value,
}

impl Plan {
    pub fn new(tool: String, goal: String, considerations: Vec<String>) -> Self {
        Self {
            tool,
            goal,
            considerations,
            args: default_args(),
        }
    }
}

fn default_args() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
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
    pub persona: String,
    #[serde(default)]
    pub applies_to: Vec<String>,
    #[serde(default)]
    pub context: Vec<ContextItem>,
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
