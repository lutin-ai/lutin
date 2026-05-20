use std::path::PathBuf;
use std::sync::Arc;

use lutin_llm::{LlmProvider, Message, ModelId};
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
    pub state_dir: PathBuf,
    pub resolver: Arc<Resolver>,
    pub events: broadcast::Sender<ChatEvent>,
}

#[derive(Debug, Clone)]
pub enum TurnOutcome {
    Yield { reply: String },
}

#[derive(Debug, Clone)]
pub enum Verdict {
    Pass,
    Fix(String),
    Rethink(String),
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
