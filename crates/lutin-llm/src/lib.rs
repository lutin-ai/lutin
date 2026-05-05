pub mod anthropic;
pub mod context;
pub mod ids;
pub mod mock;
pub mod ollama;
pub mod openai_compat;
pub mod openrouter;
pub mod types;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

pub use ids::*;
pub use types::*;

/// Errors from LLM provider operations.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after: Option<std::time::Duration>,
    },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Stream error: {0}")]
    Stream(String),

    #[error("{0}")]
    Other(String),
}

/// A stream of events from a streaming completion.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>;

/// Synthesize an [`EventStream`] from a non-streaming [`CompletionResponse`].
///
/// Lets non-streaming provider calls flow through the same consumer that
/// handles real streams, so the session loop and UI see a uniform event
/// sequence regardless of transport.
pub fn completion_as_event_stream(resp: CompletionResponse) -> EventStream {
    use futures::stream;

    let mut events: Vec<Result<StreamEvent, LlmError>> = Vec::new();

    // Suppress empty-string provider names — OpenRouter occasionally returns
    // `provider: ""` in non-streaming responses; downstream consumers expect a
    // real upstream label or no event at all.
    if let Some(provider) = resp.provider {
        if !provider.as_str().is_empty() {
            events.push(Ok(StreamEvent::Provider(provider)));
        }
    }

    // Drop empty thinking blocks — providers may return `thinking: Some("")`
    // when reasoning was enabled but the model produced none.
    if let Some(thinking) = resp.thinking {
        if !thinking.is_empty() {
            events.push(Ok(StreamEvent::Reasoning(thinking)));
        }
    }

    // Skip empty Delta — tool-only assistant turns have no visible text.
    if !resp.text.is_empty() {
        events.push(Ok(StreamEvent::Delta(resp.text)));
    }

    for tc in resp.tool_calls {
        events.push(Ok(StreamEvent::ToolCallStart {
            id: tc.id.clone(),
            name: tc.name,
        }));
        let args = if tc.arguments.is_null() {
            String::new()
        } else {
            serde_json::to_string(&tc.arguments).unwrap_or_default()
        };
        // Don't emit a ToolCallDelta for no-arg calls — `arguments` may be
        // Null or an empty object string the consumer would treat as a partial.
        if !args.is_empty() {
            events.push(Ok(StreamEvent::ToolCallDelta {
                id: tc.id,
                arguments: args,
            }));
        }
    }

    let usage = if resp.usage.total_tokens == 0
        && resp.usage.prompt_tokens == 0
        && resp.usage.completion_tokens == 0
    {
        None
    } else {
        Some(resp.usage)
    };
    events.push(Ok(StreamEvent::Done { usage }));

    Box::pin(stream::iter(events))
}

/// Trait that all LLM providers implement.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a completion request and get a full response.
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Send a completion request and get a stream of events.
    async fn stream(&self, request: CompletionRequest) -> Result<EventStream, LlmError>;

    /// List available models.
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError>;
}
