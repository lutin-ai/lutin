use serde::{Deserialize, Serialize};

use super::ids::{CallId, ModelId, ProviderName, ToolName};

/// How much reasoning effort to apply. Inlined here (was previously
/// `shared::dto::persona::ReasoningEffort`) so this crate stands alone;
/// consumers that have their own equivalent type should convert at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
}

/// A tool parameter definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameter {
    pub name: String,
    pub r#type: String,
    pub description: String,
    pub required: bool,
}

/// A tool that can be called by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: ToolName,
    pub description: String,
    pub parameters: Vec<ToolParameter>,
}

/// A tool call made by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: CallId,
    pub name: ToolName,
    pub arguments: serde_json::Value,
}

/// A tool result to send back to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultContent {
    pub call_id: CallId,
    pub content: String,
    pub is_error: bool,
}

/// A single image item, held as base64-encoded bytes with MIME type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageItem {
    pub mime: String,
    pub base64: String,
}

/// A message in a conversation — each variant carries only the data relevant to that role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
        thinking: Option<String>,
    },
    ToolResult(ToolResultContent),
    /// Standalone image content — emitted after a tool result that produced
    /// images, so the model can see them. Serialised as a multipart user
    /// message at the wire layer.
    Image { items: Vec<ImageItem> },
    /// Sub-agent reply / failure injected into the parent's transcript.
    /// Provider serializers emit each as a user-role turn with bracketed
    /// attribution (`[agent#N response]\n{text}` / `[agent#N failed:
    /// {reason}]`) — the LLM API has no role for sub-agent output so
    /// attribution is carried in-band.
    SubAgentReply { agent_id: String, text: String },
    SubAgentFailure { agent_id: String, reason: String },
    /// Synthetic message produced by compaction — replaces a contiguous
    /// prefix of older messages with a single condensed summary so the
    /// model still sees what was dropped. Providers serialize this as a
    /// user-role turn with bracketed attribution.
    Summary { text: String },
}

/// A request to the LLM.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub temperature: Option<f32>,
    /// OpenAI-style presence penalty in [-2.0, 2.0]. Honoured by all
    /// OpenAI-compatible providers (Ollama, OpenRouter, generic compat).
    /// Ignored by Anthropic (the API does not accept it).
    pub presence_penalty: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Enable extended thinking/reasoning. Honoured by Anthropic and
    /// OpenRouter; other providers ignore.
    pub thinking_enabled: bool,
    /// Provider-specific knobs. Each provider reads only what it understands;
    /// fields not consumed by the chosen provider are silently dropped — the
    /// leak is named and contained here rather than mixed into core fields.
    pub extensions: Extensions,
}

// Why: Option A — single bag of provider-specific knobs. Defaults to empty so
// callers only set what they need. See `CompletionRequest::extensions`.
/// Optional, provider-specific request knobs. None of these are guaranteed to
/// be honoured by every provider — see each field's doc for support.
#[derive(Debug, Clone, Default)]
pub struct Extensions {
    /// Reasoning controls. Honoured by Anthropic (uses `max_tokens` as the
    /// thinking budget) and OpenRouter (forwards both `effort` and
    /// `max_tokens` upstream). Ignored by Ollama and the generic
    /// OpenAI-compatible path.
    pub reasoning: Option<Reasoning>,
    /// Structured-output constraint. OpenRouter-only today (forwarded to most
    /// OpenAI-compatible upstreams). `None` keeps free-form output.
    pub response_format: Option<ResponseFormat>,
    /// Provider-level routing: upstreams to exclude. OpenRouter-only (maps to
    /// `provider.ignore`). Lets the session layer rotate away from a flaky
    /// upstream after an empty response without blacklisting the model.
    pub ignore_providers: Vec<ProviderName>,
}

/// Reasoning / extended-thinking controls.
#[derive(Debug, Clone, Default)]
pub struct Reasoning {
    /// How much reasoning effort to apply.
    pub effort: ReasoningEffort,
    /// Hard cap on reasoning tokens. `None` = let the provider decide.
    pub max_tokens: Option<u32>,
}

/// Constrained-output modes. Wire-level support is provider-specific; see
/// each provider's `build_body` for translation rules.
#[derive(Debug, Clone)]
pub enum ResponseFormat {
    /// Any syntactically valid JSON object. Maps to OpenAI/OpenRouter
    /// `response_format: {"type": "json_object"}`.
    JsonObject,
    /// Strict schema enforcement. Maps to OpenAI/OpenRouter
    /// `response_format: {"type": "json_schema", "json_schema": {...}}`.
    JsonSchema {
        name: String,
        schema: serde_json::Value,
        strict: bool,
    },
}

/// A non-streaming completion response.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    /// The assistant's text response.
    pub text: String,
    /// Reasoning / extended-thinking content, if the provider returned any.
    pub thinking: Option<String>,
    /// Tool calls made by the assistant (empty if none).
    pub tool_calls: Vec<ToolCall>,
    pub model: ModelId,
    pub usage: Usage,
    /// Upstream that actually served the request (OpenRouter sets this).
    pub provider: Option<ProviderName>,
}

/// Token usage info.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// A chunk from a streaming response.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A piece of reasoning/thinking content.
    Reasoning(String),
    /// A piece of text content.
    Delta(String),
    /// A tool call is being built up.
    ToolCallStart { id: CallId, name: ToolName },
    /// More arguments for the current tool call.
    ToolCallDelta { id: CallId, arguments: String },
    /// Stream is done; final usage info.
    Done { usage: Option<Usage> },
    /// Identifies the upstream that is serving this stream. Emitted when the
    /// provider first surfaces it (OpenRouter includes it on every chunk).
    Provider(ProviderName),
}

/// Info about a model available from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub name: String,
    pub context_length: Option<u64>,
}

