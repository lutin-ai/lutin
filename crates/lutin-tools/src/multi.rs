//! Compose multiple `Tool`s into a `Toolbox` that routes by tool name.
//!
//! Each constituent tool owns one tool name. Duplicates are rejected at
//! construction time, so the composite never has to choose between two
//! implementations at call time.

use std::collections::HashMap;

use lutin_llm::{ToolCall, ToolDefinition, ToolName};
use thiserror::Error;

use crate::{Tool, ToolCallContext, ToolError, ToolResult};

/// Construction-time errors. The dispatch path itself is infallible once
/// the toolbox is built.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("duplicate tool name: {0}")]
    DuplicateName(String),
}

pub struct Toolbox {
    tools: Vec<Box<dyn Tool>>,
    /// Tool name → index into `tools`. Built once at construction; dispatch
    /// is a single hash lookup.
    routes: HashMap<ToolName, usize>,
    /// Cached collection of every constituent's `definition()`.
    schemas: Vec<ToolDefinition>,
}

impl Toolbox {
    /// Compose `tools` into a single routing toolbox. Errors if any tool
    /// name appears more than once across the set.
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Result<Self, BuildError> {
        let mut routes: HashMap<ToolName, usize> = HashMap::new();
        let mut schemas: Vec<ToolDefinition> = Vec::new();
        for (idx, tool) in tools.iter().enumerate() {
            let schema = tool.definition();
            if routes.contains_key(&schema.name) {
                return Err(BuildError::DuplicateName(schema.name.into_inner()));
            }
            routes.insert(schema.name.clone(), idx);
            schemas.push(schema);
        }
        Ok(Self {
            tools,
            routes,
            schemas,
        })
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.schemas.clone()
    }

    pub async fn call(&self, ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        match self.routes.get(&call.name) {
            Some(&idx) => self.tools[idx].call(ctx, call).await,
            None => ToolResult::Err(ToolError::NotFound(call.name.into_inner())),
        }
    }
}
