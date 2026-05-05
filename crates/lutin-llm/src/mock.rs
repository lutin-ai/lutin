//! Mock LLM provider for testing.
//!
//! Returns a configurable sequence of canned responses. Each call to `stream()`
//! pops the next response from the queue and converts it into a stream of events.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream;

use super::{
    CompletionRequest, CompletionResponse, EventStream, LlmError, LlmProvider, ModelInfo,
    StreamEvent, ToolCall, Usage,
};

/// A single canned response the mock provider will return.
#[derive(Debug, Clone)]
pub struct MockResponse {
    /// Text content the "assistant" produces.
    pub text: String,
    /// Tool calls the "assistant" makes (empty = no tool calls).
    pub tool_calls: Vec<ToolCall>,
    /// If set, the stream will emit this error instead of a normal response.
    pub stream_error: Option<MockStreamError>,
}

/// An error to inject into the mock stream.
#[derive(Debug, Clone)]
pub struct MockStreamError {
    pub status: u16,
    pub message: String,
}

impl MockResponse {
    /// Response with only text, no tool calls.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_calls: Vec::new(),
            stream_error: None,
        }
    }

    /// Response with a single tool call and no text.
    pub fn tool_call(id: impl Into<String>, name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: crate::ids::CallId::new(id),
                name: crate::ids::ToolName::new(name),
                arguments,
            }],
            stream_error: None,
        }
    }

    /// Response with text and tool calls.
    pub fn with_tool_calls(
        text: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self {
            text: text.into(),
            tool_calls,
            stream_error: None,
        }
    }

    /// Response that fails the stream with an API error.
    pub fn error(status: u16, message: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            tool_calls: Vec::new(),
            stream_error: Some(MockStreamError {
                status,
                message: message.into(),
            }),
        }
    }

    /// Convert to a sequence of stream events.
    pub(crate) fn to_events(&self) -> Vec<Result<StreamEvent, LlmError>> {
        if let Some(ref err) = self.stream_error {
            return vec![Err(LlmError::Api {
                status: err.status,
                message: err.message.clone(),
            })];
        }

        let mut events = Vec::new();

        if !self.text.is_empty() {
            events.push(Ok(StreamEvent::Delta(self.text.clone())));
        }

        for tc in &self.tool_calls {
            events.push(Ok(StreamEvent::ToolCallStart {
                id: tc.id.clone(),
                name: tc.name.clone(),
            }));
            let args_str = serde_json::to_string(&tc.arguments).unwrap_or_default();
            events.push(Ok(StreamEvent::ToolCallDelta {
                id: tc.id.clone(),
                arguments: args_str,
            }));
        }

        events.push(Ok(StreamEvent::Done { usage: None }));
        events
    }
}

/// A mock LLM provider that returns pre-configured responses.
///
/// Each call to `stream()` or `complete()` pops the next response from the
/// queue. If the queue is empty, returns a simple "no more responses" text.
pub struct MockProvider {
    responses: Mutex<VecDeque<MockResponse>>,
}

impl MockProvider {
    /// Create a mock provider with a sequence of responses.
    pub fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
        }
    }

    /// Create a mock that always returns the same text.
    pub fn always_text(text: impl Into<String>) -> Self {
        let text = text.into();
        // Return 50 copies — enough for any reasonable test.
        let responses = (0..50).map(|_| MockResponse::text(text.clone())).collect();
        Self::new(responses)
    }

    fn pop_response(&self) -> MockResponse {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| MockResponse::text("[mock: no more responses]"))
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let resp = self.pop_response();
        if let Some(ref err) = resp.stream_error {
            return Err(LlmError::Api {
                status: err.status,
                message: err.message.clone(),
            });
        }
        Ok(CompletionResponse {
            text: resp.text,
            thinking: None,
            tool_calls: resp.tool_calls,
            model: "mock-model".into(),
            usage: Usage::default(),
            provider: None,
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> Result<EventStream, LlmError> {
        let resp = self.pop_response();
        let events = resp.to_events();
        Ok(Box::pin(stream::iter(events)))
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![ModelInfo {
            id: "mock-model".into(),
            name: "Mock Model".into(),
            context_length: Some(128_000),
        }])
    }
}
