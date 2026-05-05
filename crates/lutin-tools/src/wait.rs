use std::time::Duration;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::outcome::ToolOutput;

pub struct Wait;

impl Wait {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Wait {
    fn default() -> Self {
        Self::new()
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("wait"),
        description: "Pause execution for a specified number of seconds (1-300). Useful in workflows for delays or polling intervals.".into(),
        parameters: vec![ToolParameter {
            name: "seconds".into(),
            r#type: "integer".into(),
            description: "Number of seconds to wait (clamped to 1-300).".into(),
            required: true,
        }],
    }
}

#[derive(Deserialize)]
struct Input {
    seconds: u64,
}

#[async_trait]
impl crate::Tool for Wait {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = match serde_json::from_value::<Input>(call.arguments) {
            Ok(Input { seconds }) => {
                let seconds = seconds.clamp(1, 300);
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                ToolOutput::ok(format!("waited {seconds} seconds"))
            }
            Err(e) => ToolOutput::err(format!("invalid input: {e}")),
        };
        out.into_outcome(call.id)
    }
}
