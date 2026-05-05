#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Mutex;

use lutin_agent_sdk::{Tool, ToolCallContext, ToolResult, Toolbox};
use async_trait::async_trait;
use lutin_llm::{
    CompletionRequest, CompletionResponse, EventStream, LlmError, LlmProvider, ModelInfo,
    StreamEvent, ToolCall, ToolDefinition, ToolName, Usage,
};

pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("echo"),
            description: String::new(),
            parameters: Vec::new(),
        }
    }
    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        ToolResult::Ok(lutin_llm::ToolResultContent {
            call_id: call.id,
            content: "ok".into(),
            is_error: false,
        })
    }
}

pub struct TrapTool;

#[async_trait]
impl Tool for TrapTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("echo"),
            description: String::new(),
            parameters: Vec::new(),
        }
    }
    async fn call(&self, _ctx: &ToolCallContext, _call: ToolCall) -> ToolResult {
        panic!("tool must not be dispatched on invalid args");
    }
}

/// Build a Toolbox containing a single tool. Tests previously passed
/// `Box::new(EchoTool)` to `try_set_tools`; with the Toolbox API they need
/// to wrap their tool in a composite first.
pub fn toolbox_of<T: Tool + 'static>(tool: T) -> Toolbox {
    Toolbox::new(vec![Box::new(tool)]).expect("single-tool toolbox cannot duplicate")
}

pub struct BadArgsProvider;

#[async_trait]
impl LlmProvider for BadArgsProvider {
    async fn complete(&self, _r: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        unreachable!()
    }
    async fn stream(&self, _r: CompletionRequest) -> Result<EventStream, LlmError> {
        use futures::stream;
        let id = lutin_llm::CallId::new("c1");
        let events: Vec<Result<StreamEvent, LlmError>> = vec![
            Ok(StreamEvent::ToolCallStart {
                id: id.clone(),
                name: lutin_llm::ToolName::new("echo"),
            }),
            Ok(StreamEvent::ToolCallDelta {
                id,
                arguments: "{not json".into(),
            }),
            Ok(StreamEvent::Done { usage: None }),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![])
    }
}

pub struct UsageProvider {
    pub responses: Mutex<VecDeque<(String, bool, Usage)>>,
}

#[async_trait]
impl LlmProvider for UsageProvider {
    async fn complete(&self, _r: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        unreachable!()
    }
    async fn stream(&self, _r: CompletionRequest) -> Result<EventStream, LlmError> {
        use futures::stream;
        let (text, with_tool, usage) = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("no more responses");
        let mut events: Vec<Result<StreamEvent, LlmError>> = Vec::new();
        if !text.is_empty() {
            events.push(Ok(StreamEvent::Delta(text)));
        }
        if with_tool {
            let id = lutin_llm::CallId::new("c1");
            events.push(Ok(StreamEvent::ToolCallStart {
                id: id.clone(),
                name: lutin_llm::ToolName::new("echo"),
            }));
            events.push(Ok(StreamEvent::ToolCallDelta {
                id,
                arguments: "{}".into(),
            }));
        }
        events.push(Ok(StreamEvent::Done { usage: Some(usage) }));
        Ok(Box::pin(stream::iter(events)))
    }
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![])
    }
}
