//! Shared types and helpers for OpenAI-compatible LLM APIs.
//!
//! Both [`super::openrouter`] and [`super::ollama`] use the OpenAI chat
//! completions wire format. This module provides the common serialisation
//! types, message/tool conversion, and SSE stream parsing so that each
//! provider only needs to handle authentication, endpoint URLs, and
//! provider-specific extensions.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{
    CompletionRequest, LlmError, Message, ReasoningEffort, ResponseFormat, StreamEvent, ToolCall,
    Usage,
};

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ApiRequest {
    pub model: String,
    pub messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ApiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    /// Asks the provider to include a final usage chunk in the SSE stream.
    /// Required by OpenAI-compat providers to emit token counts while streaming.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// Provider-specific extra fields merged into the top-level JSON object.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Serialize)]
pub struct ApiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ApiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ApiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// OpenAI-compatible message content: either a plain string or an array of
/// typed parts (used for vision / image content).
#[derive(Serialize)]
#[serde(untagged)]
pub enum ApiContent {
    Text(String),
    Parts(Vec<ApiContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
pub enum ApiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ApiImageUrl },
}

#[derive(Serialize)]
pub struct ApiImageUrl {
    /// `data:<mime>;base64,<payload>` URL.
    pub url: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ApiToolCall {
    pub id: String,
    pub r#type: String,
    pub function: ApiFunction,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ApiFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize)]
pub struct ApiTool {
    pub r#type: String,
    pub function: ApiToolDef,
}

#[derive(Serialize)]
pub struct ApiToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ApiResponse {
    pub choices: Vec<ApiChoice>,
    pub model: Option<String>,
    pub usage: Option<ApiUsage>,
    /// OpenRouter reports the upstream that served the request here.
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Deserialize)]
pub struct ApiChoice {
    pub message: Option<ApiResponseMessage>,
}

#[derive(Deserialize)]
pub struct ApiResponseMessage {
    pub content: Option<String>,
    /// OpenRouter-style reasoning field.
    #[serde(default)]
    pub reasoning: Option<String>,
    /// DeepSeek / some OpenAI-compat providers expose reasoning here.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct ApiUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ApiError {
    pub error: ApiErrorDetail,
}

#[derive(Deserialize)]
pub struct ApiErrorDetail {
    pub message: String,
    #[serde(default)]
    pub code: Option<serde_json::Value>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

impl ApiErrorDetail {
    /// Build a human-readable message including code/metadata when present.
    pub fn full_message(&self) -> String {
        let mut msg = self.message.clone();
        if let Some(code) = &self.code {
            msg = format!("{msg} (code: {code})");
        }
        if let Some(meta) = &self.metadata {
            msg = format!("{msg} [metadata: {meta}]");
        }
        msg
    }
}

// ---------------------------------------------------------------------------
// Streaming delta types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StreamChunk {
    pub choices: Vec<StreamChoice>,
    pub usage: Option<ApiUsage>,
    /// OpenRouter includes the serving upstream on each chunk.
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub delta: Option<StreamDeltaMessage>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamDeltaMessage {
    pub content: Option<String>,
    /// OpenRouter-style reasoning field.
    #[serde(default)]
    pub reasoning: Option<String>,
    /// MiniMax / DeepSeek / sglang-backed providers stream reasoning here.
    /// Without this field, minimax-m2.x reasoning deltas silently drop and
    /// the stream appears to end with an empty assistant turn.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
pub struct StreamToolCallDelta {
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    pub function: Option<StreamFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub struct StreamFunctionDelta {
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// Models list response
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<ApiModel>,
}

#[derive(Deserialize)]
pub struct ApiModel {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<u64>,
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Convert internal [`Message`] list to the OpenAI wire format.
///
/// Drops assistant messages that carry neither text nor tool_calls — some
/// models (e.g. minimax-m2.7) emit these as stray turns and certain providers
/// reject subsequent requests that include them.
pub fn convert_messages(messages: Vec<Message>) -> Vec<ApiMessage> {
    let before = messages.len();
    let filtered: Vec<Message> = messages
        .into_iter()
        .filter(|m| match m {
            Message::Assistant { text, tool_calls, .. } => {
                let empty = text.trim().is_empty() && tool_calls.is_empty();
                if empty {
                    log::warn!("dropping empty assistant message (no text, no tool_calls)");
                }
                !empty
            }
            _ => true,
        })
        .collect();
    if filtered.len() != before {
        log::debug!(
            "convert_messages: filtered {} → {} messages",
            before,
            filtered.len()
        );
    }
    filtered
        .into_iter()
        .map(|m| match m {
            Message::System(text) => ApiMessage {
                role: "system".into(),
                content: Some(ApiContent::Text(text)),
                tool_calls: None,
                tool_call_id: None,
            },
            Message::User(text) => ApiMessage {
                role: "user".into(),
                content: Some(ApiContent::Text(text)),
                tool_calls: None,
                tool_call_id: None,
            },
            Message::Assistant {
                text, tool_calls, ..
            } => {
                let api_tool_calls = if tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        tool_calls
                            .into_iter()
                            .map(|tc| ApiToolCall {
                                id: tc.id.into_inner(),
                                r#type: "function".into(),
                                function: ApiFunction {
                                    name: tc.name.into_inner(),
                                    arguments: tc.arguments.to_string(),
                                },
                            })
                            .collect(),
                    )
                };
                // Drop whitespace-only text when tool_calls are present —
                // some providers reject/ignore responses where trailing
                // assistant content is just whitespace.
                let content = if text.is_empty()
                    || (api_tool_calls.is_some() && text.trim().is_empty())
                {
                    None
                } else {
                    Some(ApiContent::Text(text))
                };
                ApiMessage {
                    role: "assistant".into(),
                    content,
                    tool_calls: api_tool_calls,
                    tool_call_id: None,
                }
            }
            Message::ToolResult(tr) => ApiMessage {
                role: "tool".into(),
                content: Some(ApiContent::Text(tr.content)),
                tool_calls: None,
                tool_call_id: Some(tr.call_id.into_inner()),
            },
            Message::Image { items } => {
                // Emit as a user message with multipart image_url content.
                // Tool-result image content isn't portable across OpenAI-compat
                // providers, but a following user message with image_url parts is.
                let parts: Vec<ApiContentPart> = items
                    .into_iter()
                    .map(|img| ApiContentPart::ImageUrl {
                        image_url: ApiImageUrl {
                            url: format!("data:{};base64,{}", img.mime, img.base64),
                        },
                    })
                    .collect();
                ApiMessage {
                    role: "user".into(),
                    content: Some(ApiContent::Parts(parts)),
                    tool_calls: None,
                    tool_call_id: None,
                }
            }
        })
        .collect()
}

/// Convert internal tool definitions to the OpenAI function-calling wire format.
pub fn convert_tools(tools: &[super::ToolDefinition]) -> Vec<ApiTool> {
    tools
        .iter()
        .map(|t| {
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();

            for p in &t.parameters {
                let mut prop = serde_json::Map::new();
                prop.insert("type".into(), serde_json::Value::String(p.r#type.clone()));
                prop.insert(
                    "description".into(),
                    serde_json::Value::String(p.description.clone()),
                );
                properties.insert(p.name.clone(), serde_json::Value::Object(prop));
                if p.required {
                    required.push(serde_json::Value::String(p.name.clone()));
                }
            }

            ApiTool {
                r#type: "function".into(),
                function: ApiToolDef {
                    name: t.name.as_str().to_string(),
                    description: t.description.clone(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": properties,
                        "required": required,
                    }),
                },
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Request builder
// ---------------------------------------------------------------------------

/// Per-provider toggles for OpenAI-compatible request bodies.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAiCompatQuirks {
    /// Emit a top-level `reasoning` object (OpenRouter). When `thinking_enabled`
    /// is false, an explicit `{"enabled": false}` object is sent — some
    /// upstreams (minimax-m2.x) reason by default unless told otherwise.
    pub include_reasoning: bool,
    /// Forward `extensions.response_format` as a top-level `response_format`.
    pub include_response_format: bool,
    /// Emit `provider.ignore` routing hints from `extensions.ignore_providers`.
    pub include_ignore_providers: bool,
}

#[derive(Serialize)]
struct ApiReasoning {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

fn reasoning_value(req: &CompletionRequest) -> serde_json::Value {
    let r = if req.thinking_enabled {
        let cfg = req.extensions.reasoning.clone().unwrap_or_default();
        ApiReasoning {
            enabled: true,
            effort: Some(match cfg.effort {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
            }),
            max_tokens: cfg.max_tokens,
        }
    } else {
        ApiReasoning {
            enabled: false,
            effort: None,
            max_tokens: None,
        }
    };
    serde_json::to_value(&r).unwrap_or(serde_json::Value::Null)
}

fn response_format_value(fmt: &ResponseFormat) -> serde_json::Value {
    match fmt {
        ResponseFormat::JsonObject => serde_json::json!({"type": "json_object"}),
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "strict": strict,
                "schema": schema,
            },
        }),
    }
}

/// Whether the built request should be streamed (SSE) or returned as a single
/// buffered response. Replaces a bare `bool` parameter at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    Stream,
    Buffered,
}

impl StreamMode {
    fn is_stream(self) -> bool {
        matches!(self, StreamMode::Stream)
    }
}

/// Build an [`ApiRequest`] from a [`CompletionRequest`], applying provider quirks.
pub fn build_request(
    request: CompletionRequest,
    mode: StreamMode,
    quirks: &OpenAiCompatQuirks,
) -> ApiRequest {
    let stream = mode.is_stream();
    let mut extra = serde_json::Map::new();

    if quirks.include_reasoning {
        extra.insert("reasoning".into(), reasoning_value(&request));
    }

    if quirks.include_ignore_providers && !request.extensions.ignore_providers.is_empty() {
        extra.insert(
            "provider".into(),
            serde_json::json!({
                "ignore": request.extensions.ignore_providers,
                "allow_fallbacks": true,
            }),
        );
    }

    if quirks.include_response_format {
        if let Some(fmt) = &request.extensions.response_format {
            extra.insert("response_format".into(), response_format_value(fmt));
        }
    }

    ApiRequest {
        model: request.model.to_string(),
        messages: convert_messages(request.messages),
        tools: convert_tools(&request.tools),
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        stream,
        stream_options: stream.then_some(StreamOptions { include_usage: true }),
        extra,
    }
}

/// Parse an [`ApiResponse`] into a [`super::CompletionResponse`].
pub fn parse_completion_response(
    api_resp: ApiResponse,
) -> Result<super::CompletionResponse, LlmError> {
    let choice = api_resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::Other("empty response".into()))?;
    let msg = choice
        .message
        .ok_or_else(|| LlmError::Other("no message in response".into()))?;

    let tool_calls = msg
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|tc| {
            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)?;
            Ok::<_, LlmError>(ToolCall {
                id: super::CallId::new(tc.id),
                name: super::ToolName::new(tc.function.name),
                arguments: args,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let usage = api_resp.usage.map(convert_usage);

    let thinking = msg
        .reasoning
        .or(msg.reasoning_content)
        .filter(|s| !s.is_empty());

    Ok(super::CompletionResponse {
        text: msg.content.unwrap_or_default(),
        thinking,
        tool_calls,
        model: super::ModelId::from(api_resp.model.unwrap_or_default()),
        usage: usage.unwrap_or_default(),
        provider: api_resp.provider.map(super::ProviderName::new),
    })
}

fn convert_usage(u: ApiUsage) -> Usage {
    Usage {
        prompt_tokens: u.prompt_tokens.unwrap_or(0),
        completion_tokens: u.completion_tokens.unwrap_or(0),
        total_tokens: u.total_tokens.unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// SSE stream parsing
// ---------------------------------------------------------------------------

/// Hard cap on per-tool-call streamed `arguments` bytes.
// Why: `pending_args` (and downstream tool-arg accumulation) is fed by an
// untrusted upstream. 8 MiB comfortably exceeds any realistic JSON tool call
// while still bounding memory if a misbehaving provider streams unbounded
// `arguments` deltas.
const MAX_TOOL_ARGS_BYTES: usize = 8 * 1024 * 1024;

/// Per-index buffer for a streaming tool call.
///
/// Some providers (e.g. Minimax via OpenRouter) emit initial tool_call deltas
/// without an `id` or `name` — and may even stream `arguments` before the
/// name arrives. We buffer by `index` so nothing is dropped, emitting the
/// `ToolCallStart` event lazily once the name is known, synthesising a
/// stable id if the provider never sends one.
#[derive(Default)]
pub struct ToolCallBuf {
    pub id: Option<String>,
    pub name: Option<String>,
    /// Arguments received before `ToolCallStart` could be emitted.
    pub pending_args: String,
    /// The id actually emitted in `ToolCallStart` — real or synthesised.
    /// Present iff `ToolCallStart` has been emitted for this index.
    pub resolved_id: Option<String>,
}

/// Process complete SSE lines from the buffer, draining each processed line.
///
/// Returns `Ok(events)` on success, or `Err(LlmError)` if the stream contains
/// an error response from the upstream provider.
pub fn process_sse_lines(
    line_buf: &mut String,
    active_tool_ids: &mut Vec<ToolCallBuf>,
    last_usage: &mut Option<Usage>,
    last_provider: &mut Option<String>,
) -> Result<Vec<StreamEvent>, LlmError> {
    let mut events = Vec::new();

    while let Some(newline_pos) = line_buf.find('\n') {
        let line = line_buf[..newline_pos].trim();

        if !line.is_empty() {
            log::trace!("SSE line: {line}");
        }

        if line == "data: [DONE]" {
            events.push(StreamEvent::Done {
                usage: last_usage.take(),
            });
        } else if let Some(data) = line.strip_prefix("data: ") {
            match serde_json::from_str::<StreamChunk>(data) {
                Ok(chunk) => {
                    let before = events.len();
                    let had_finish = chunk
                        .choices
                        .iter()
                        .any(|c| c.finish_reason.is_some());
                    let had_usage = chunk.usage.is_some();
                    let had_tool_calls = chunk.choices.iter().any(|c| {
                        c.delta
                            .as_ref()
                            .and_then(|d| d.tool_calls.as_ref())
                            .is_some_and(|t| !t.is_empty())
                    });
                    parse_chunk_events(
                        &chunk,
                        active_tool_ids,
                        &mut events,
                        last_usage,
                        last_provider,
                    )?;
                    if events.len() == before && !had_finish && !had_usage && !had_tool_calls {
                        log::warn!(
                            "SSE chunk parsed but produced no events: {chunk:?} (raw: {data})"
                        );
                    }
                }
                Err(e) => {
                    if let Ok(api_err) = serde_json::from_str::<ApiError>(data) {
                        log::error!(
                            "stream error: {}",
                            api_err.error.full_message()
                        );
                        let status = api_err
                            .error
                            .code
                            .as_ref()
                            .and_then(|c| {
                                c.as_u64()
                                    .map(|n| n as u16)
                                    .or_else(|| c.as_str().and_then(|s| s.parse().ok()))
                            })
                            .unwrap_or(500);
                        line_buf.drain(..newline_pos + 1);
                        return Err(LlmError::Api {
                            status,
                            message: api_err.error.full_message(),
                        });
                    }
                    log::warn!("skipping unparseable SSE data ({e}): {data}");
                }
            }
        }

        line_buf.drain(..newline_pos + 1);
    }

    if !line_buf.is_empty() {
        log::trace!("SSE line_buf remainder (no trailing newline): {line_buf:?}");
    }

    Ok(events)
}

/// Extract stream events from a parsed SSE chunk.
pub fn parse_chunk_events(
    chunk: &StreamChunk,
    active_tool_ids: &mut Vec<ToolCallBuf>,
    events: &mut Vec<StreamEvent>,
    last_usage: &mut Option<Usage>,
    last_provider: &mut Option<String>,
) -> Result<(), LlmError> {
    if let Some(ref p) = chunk.provider {
        if !p.is_empty() && last_provider.as_deref() != Some(p.as_str()) {
            events.push(StreamEvent::Provider(super::ProviderName::new(p.clone())));
            *last_provider = Some(p.clone());
        }
    }
    for choice in &chunk.choices {
        if let Some(ref reason) = choice.finish_reason {
            if reason != "stop" && reason != "tool_calls" {
                log::warn!("LLM stream finish_reason: {reason}");
            }
        }
        let Some(ref delta) = choice.delta else {
            continue;
        };
        let reasoning_src = delta
            .reasoning
            .as_deref()
            .or(delta.reasoning_content.as_deref());
        if let Some(reasoning) = reasoning_src {
            if !reasoning.is_empty() {
                events.push(StreamEvent::Reasoning(reasoning.to_string()));
            }
        }
        if let Some(ref content) = delta.content {
            if !content.is_empty() {
                events.push(StreamEvent::Delta(content.clone()));
            }
        }
        let Some(ref tool_calls) = delta.tool_calls else {
            continue;
        };
        for tc in tool_calls {
            let idx = tc.index;

            if idx >= active_tool_ids.len() {
                active_tool_ids.resize_with(idx + 1, ToolCallBuf::default);
            }
            let buf = &mut active_tool_ids[idx];

            if let Some(ref id) = tc.id {
                buf.id = Some(id.clone());
            }
            if let Some(ref func) = tc.function {
                if let Some(ref name) = func.name {
                    if !name.is_empty() {
                        buf.name = Some(name.clone());
                    }
                }
            }

            // Emit `ToolCallStart` as soon as we have a name. If the
            // provider never sent an `id`, synthesise one from the
            // index so downstream can still route delta chunks.
            if buf.resolved_id.is_none() {
                if let Some(ref name) = buf.name {
                    let resolved = buf.id.clone().unwrap_or_else(|| format!("call_{idx}"));
                    events.push(StreamEvent::ToolCallStart {
                        id: super::CallId::new(resolved.clone()),
                        name: super::ToolName::new(name.clone()),
                    });
                    buf.resolved_id = Some(resolved.clone());
                    if !buf.pending_args.is_empty() {
                        let args = std::mem::take(&mut buf.pending_args);
                        events.push(StreamEvent::ToolCallDelta {
                            id: super::CallId::new(resolved),
                            arguments: args,
                        });
                    }
                }
            }

            let Some(ref func) = tc.function else {
                continue;
            };
            let Some(ref args) = func.arguments else {
                continue;
            };
            if args.is_empty() {
                continue;
            }
            match &buf.resolved_id {
                Some(id) => events.push(StreamEvent::ToolCallDelta {
                    id: super::CallId::new(id.clone()),
                    arguments: args.clone(),
                }),
                None => {
                    if buf.pending_args.len().saturating_add(args.len()) > MAX_TOOL_ARGS_BYTES {
                        return Err(LlmError::Stream(format!(
                            "tool-call arguments exceeded {MAX_TOOL_ARGS_BYTES} bytes (MAX_TOOL_ARGS_BYTES) before name was streamed"
                        )));
                    }
                    buf.pending_args.push_str(args);
                }
            }
        }
    }

    if let Some(ref usage) = chunk.usage {
        *last_usage = Some(Usage {
            prompt_tokens: usage.prompt_tokens.unwrap_or(0),
            completion_tokens: usage.completion_tokens.unwrap_or(0),
            total_tokens: usage.total_tokens.unwrap_or(0),
        });
    }
    Ok(())
}

/// Build an [`EventStream`] from a `reqwest` streaming response that uses SSE.
pub fn sse_event_stream(resp: reqwest::Response) -> super::EventStream {
    use futures::{StreamExt, TryStreamExt};

    let byte_stream = resp.bytes_stream();
    let mut active_tool_ids: Vec<ToolCallBuf> = Vec::new();
    let mut last_usage: Option<Usage> = None;
    let mut last_provider: Option<String> = None;

    let event_stream = byte_stream
        .scan(String::new(), move |line_buf, chunk| {
            let result = match chunk {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    line_buf.push_str(&text);

                    match process_sse_lines(
                        line_buf,
                        &mut active_tool_ids,
                        &mut last_usage,
                        &mut last_provider,
                    ) {
                        Ok(events) => Ok::<_, LlmError>(futures::stream::iter(
                            events.into_iter().map(Ok::<_, LlmError>),
                        )),
                        Err(err) => {
                            return futures::future::ready(Some(Err(err)));
                        }
                    }
                }
                Err(e) => {
                    log::warn!("SSE byte stream error: {e} (buffered remainder: {line_buf:?})");
                    Err(LlmError::Http(e))
                }
            };
            futures::future::ready(Some(result))
        })
        .try_flatten()
        .inspect(|res| match res {
            Ok(ev) => log::trace!("stream event: {ev:?}"),
            Err(e) => log::warn!("stream yielded error: {e}"),
        });

    Box::pin(event_stream)
}

/// Read response body as text, then deserialize as JSON. On parse failure,
/// log the raw body (truncated) so the cause of "error decoding response
/// body" is visible rather than opaque.
pub async fn decode_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
    context: &str,
) -> Result<T, LlmError> {
    let text = resp.text().await?;
    serde_json::from_str::<T>(&text).map_err(|e| {
        let preview: String = text.chars().take(2000).collect();
        log::error!(
            "{context}: failed to decode JSON ({e}); body ({} bytes): {preview}",
            text.len()
        );
        LlmError::Other(format!("decode {context}: {e}"))
    })
}

/// Parse an error response body into an [`LlmError::Api`].
pub fn parse_error_body(status: u16, body: &str) -> LlmError {
    let message = serde_json::from_str::<ApiError>(body)
        .map(|e| e.error.full_message())
        .unwrap_or_else(|_| body.to_string());
    LlmError::Api { status, message }
}

/// Parse the `Retry-After` response header into a [`Duration`].
///
/// Accepts the seconds form (`"30"`) and the HTTP-date form
/// (`"Wed, 21 Oct 2026 07:28:00 GMT"`) per RFC 7231. For HTTP-date the
/// returned duration is `(date - now).max(ZERO)`.
///
/// Why: malformed/missing headers return `None` rather than erroring —
/// the caller should fall back to its own backoff policy.
pub fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let val = resp.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let val = val.trim();
    if let Ok(secs) = val.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date (IMF-fixdate is RFC 2822-compatible for the canonical form).
    let dt = chrono::DateTime::parse_from_rfc2822(val).ok()?;
    let delta = dt.with_timezone(&chrono::Utc) - chrono::Utc::now();
    delta.to_std().ok().or(Some(Duration::ZERO))
}

/// Build an [`LlmError`] from a non-success response, classifying 429s as
/// [`LlmError::RateLimited`] (with `Retry-After` parsed) and everything else
/// as [`LlmError::Api`]. Consumes the response body.
pub async fn error_from_response(resp: reqwest::Response) -> LlmError {
    let status = resp.status().as_u16();
    let retry_after = parse_retry_after(&resp);
    let body = resp.text().await.unwrap_or_default();
    if status == 429 {
        let message = serde_json::from_str::<ApiError>(&body)
            .map(|e| e.error.full_message())
            .unwrap_or_else(|_| body.clone());
        return LlmError::RateLimited {
            message,
            retry_after,
        };
    }
    parse_error_body(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // process_sse_lines
    // -----------------------------------------------------------------------

    #[test]
    fn sse_text_delta() {
        let mut buf = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n".to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Delta(t) if t == "hello"));
        assert!(buf.is_empty(), "buffer should be drained");
    }

    #[test]
    fn sse_done_marker() {
        let mut buf = "data: [DONE]\n".to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Done { .. }));
    }

    #[test]
    fn sse_error_with_numeric_code() {
        let mut buf =
            "data: {\"error\":{\"message\":\"Rate limit exceeded\",\"code\":429}}\n".to_string();
        let mut ids = Vec::new();
        let err = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap_err();
        match err {
            LlmError::Api { status, message } => {
                assert_eq!(status, 429);
                assert!(message.contains("Rate limit"), "got: {message}");
            }
            other => panic!("expected Api error, got: {other}"),
        }
    }

    #[test]
    fn sse_error_with_string_code() {
        let mut buf =
            "data: {\"error\":{\"message\":\"Too many requests\",\"code\":\"429\"}}\n".to_string();
        let mut ids = Vec::new();
        let err = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap_err();
        match err {
            LlmError::Api { status, message } => {
                assert_eq!(status, 429);
                assert!(message.contains("Too many requests"), "got: {message}");
            }
            other => panic!("expected Api error, got: {other}"),
        }
    }

    #[test]
    fn sse_error_without_code_defaults_to_500() {
        let mut buf =
            "data: {\"error\":{\"message\":\"Internal server error\"}}\n".to_string();
        let mut ids = Vec::new();
        let err = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap_err();
        match err {
            LlmError::Api { status, message } => {
                assert_eq!(status, 500);
                assert!(message.contains("Internal server error"), "got: {message}");
            }
            other => panic!("expected Api error, got: {other}"),
        }
    }

    #[test]
    fn sse_error_with_metadata() {
        let mut buf = concat!(
            "data: {\"error\":{\"message\":\"Content filtered\",",
            "\"code\":400,\"metadata\":{\"reason\":\"safety\"}}}\n"
        )
        .to_string();
        let mut ids = Vec::new();
        let err = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap_err();
        match err {
            LlmError::Api { status, message } => {
                assert_eq!(status, 400);
                assert!(message.contains("Content filtered"), "got: {message}");
                assert!(message.contains("metadata"), "metadata should be included: {message}");
            }
            other => panic!("expected Api error, got: {other}"),
        }
    }

    #[test]
    fn sse_events_before_error_are_preserved() {
        let mut buf = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n",
            "data: {\"error\":{\"message\":\"rate limited\",\"code\":429}}\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let err = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap_err();
        assert!(matches!(err, LlmError::Api { status: 429, .. }));
    }

    #[test]
    fn sse_unparseable_non_error_data_is_skipped() {
        let mut buf = concat!(
            "data: {\"unknown_field\": true}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Delta(t) if t == "hello"));
    }

    #[test]
    fn sse_empty_lines_and_comments_are_skipped() {
        let mut buf = "\n: this is a comment\ndata: [DONE]\n".to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Done { .. }));
    }

    #[test]
    fn sse_partial_line_stays_in_buffer() {
        let mut buf = "data: {\"choices\":[{\"de".to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();
        assert!(events.is_empty(), "partial line should not produce events");
        assert_eq!(buf, "data: {\"choices\":[{\"de", "partial line should stay in buffer");
    }

    #[test]
    fn sse_multiple_chunks_reassemble() {
        let mut buf = "data: {\"choices\":[{\"de".to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();
        assert!(events.is_empty());

        buf.push_str("lta\":{\"content\":\"hi\"}}]}\n");
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Delta(t) if t == "hi"));
        assert!(buf.is_empty());
    }

    #[test]
    fn sse_tool_call_start_and_delta() {
        let mut buf = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"tc-1\",\"function\":{\"name\":\"web_search\",\"arguments\":\"\"}}",
            "]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"function\":{\"arguments\":\"{\\\"query\\\":\\\"rust\\\"}\"}}",
            "]}}]}\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::ToolCallStart { id, name }
            if id == "tc-1" && name == "web_search"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { id, arguments }
            if id == "tc-1" && arguments.contains("rust")));
    }

    /// Minimax (via OpenRouter) opens tool_call streams with a stub chunk that
    /// has no `id` and no `name`, only an empty `arguments: ""`. The name and
    /// id arrive in subsequent chunks. Prior versions silently dropped the
    /// whole tool call in this pattern — regression-guard.
    #[test]
    fn sse_tool_call_minimax_pattern_preamble_then_name() {
        let mut buf = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"function\":{\"arguments\":\"\"}}",
            "]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"tc-x\",\"function\":{\"name\":\"shell\"}}",
            "]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}",
            "]}}]}\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();

        assert_eq!(events.len(), 2, "got: {events:?}");
        assert!(matches!(&events[0], StreamEvent::ToolCallStart { id, name }
            if id == "tc-x" && name == "shell"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { id, arguments }
            if id == "tc-x" && arguments.contains("ls")));
    }

    /// If a provider streams `arguments` *before* the name, those arguments
    /// must be buffered and flushed once `ToolCallStart` is emitted.
    #[test]
    fn sse_tool_call_args_before_name_are_buffered() {
        let mut buf = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"function\":{\"arguments\":\"{\\\"a\\\":\"}}",
            "]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"t1\",\"function\":{\"name\":\"f\",\"arguments\":\"1}\"}}",
            "]}}]}\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();

        assert_eq!(events.len(), 3, "got: {events:?}");
        assert!(matches!(&events[0], StreamEvent::ToolCallStart { id, name }
            if id == "t1" && name == "f"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { id, arguments }
            if id == "t1" && arguments == "{\"a\":"));
        assert!(matches!(&events[2], StreamEvent::ToolCallDelta { id, arguments }
            if id == "t1" && arguments == "1}"));
    }

    /// If a provider never sends an `id`, synthesise `call_{index}` so the
    /// call can still be dispatched rather than being silently dropped.
    #[test]
    fn sse_tool_call_missing_id_synthesises_one() {
        let mut buf = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}",
            "]}}]}\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let events = process_sse_lines(&mut buf, &mut ids, &mut None, &mut None).unwrap();

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::ToolCallStart { id, name }
            if id == "call_0" && name == "f"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { id, arguments }
            if id == "call_0" && arguments == "{}"));
    }

    /// OpenRouter includes `"provider": "<upstream>"` on each chunk. We emit
    /// a `StreamEvent::Provider` once, dedup across subsequent chunks.
    #[test]
    fn sse_provider_field_emitted_once() {
        let mut buf = concat!(
            "data: {\"provider\":\"Minimax\",\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
            "data: {\"provider\":\"Minimax\",\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let mut last_provider = None;
        let events =
            process_sse_lines(&mut buf, &mut ids, &mut None, &mut last_provider).unwrap();
        let provider_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Provider(_)))
            .collect();
        assert_eq!(provider_events.len(), 1, "got: {events:?}");
        assert!(matches!(&events[0], StreamEvent::Provider(p) if p == "Minimax"));
        assert_eq!(last_provider.as_deref(), Some("Minimax"));
    }

    #[test]
    fn sse_usage_chunk_stores_usage_for_done() {
        let mut buf = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"bye\"}}],",
            "\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n",
            "data: [DONE]\n",
        )
        .to_string();
        let mut ids = Vec::new();
        let mut usage_store = None;
        let events = process_sse_lines(&mut buf, &mut ids, &mut usage_store, &mut None).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::Delta(t) if t == "bye"));
        match &events[1] {
            StreamEvent::Done {
                usage: Some(usage), ..
            } => {
                assert_eq!(usage.prompt_tokens, 10);
                assert_eq!(usage.completion_tokens, 5);
                assert_eq!(usage.total_tokens, 15);
            }
            other => panic!("expected Done with usage, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // parse_chunk_events
    // -----------------------------------------------------------------------

    #[test]
    fn parse_chunk_empty_choices() {
        let chunk = StreamChunk {
            choices: vec![],
            usage: None,
            provider: None,
        };
        let mut ids = Vec::new();
        let mut events = Vec::new();
        parse_chunk_events(&chunk, &mut ids, &mut events, &mut None, &mut None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_chunk_choice_with_no_delta() {
        let chunk: StreamChunk = serde_json::from_str(r#"{"choices":[{}]}"#).unwrap();
        let mut ids = Vec::new();
        let mut events = Vec::new();
        parse_chunk_events(&chunk, &mut ids, &mut events, &mut None, &mut None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_chunk_multiple_tool_calls_different_indices() {
        let chunk: StreamChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"a","function":{"name":"file_read","arguments":""}},
                {"index":1,"id":"b","function":{"name":"file_write","arguments":""}}
            ]}}]}"#,
        )
        .unwrap();
        let mut ids = Vec::new();
        let mut events = Vec::new();
        parse_chunk_events(&chunk, &mut ids, &mut events, &mut None, &mut None).unwrap();

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::ToolCallStart { name, .. } if name == "file_read"));
        assert!(matches!(&events[1], StreamEvent::ToolCallStart { name, .. } if name == "file_write"));
        assert_eq!(ids.len(), 2);
    }

    // -----------------------------------------------------------------------
    // ApiErrorDetail::full_message
    // -----------------------------------------------------------------------

    #[test]
    fn api_error_detail_message_only() {
        let detail = ApiErrorDetail {
            message: "something broke".into(),
            code: None,
            metadata: None,
        };
        assert_eq!(detail.full_message(), "something broke");
    }

    #[test]
    fn api_error_detail_with_code_and_metadata() {
        let detail = ApiErrorDetail {
            message: "rate limited".into(),
            code: Some(serde_json::json!(429)),
            metadata: Some(serde_json::json!({"provider": "openai"})),
        };
        let msg = detail.full_message();
        assert!(msg.contains("rate limited"));
        assert!(msg.contains("429"));
        assert!(msg.contains("openai"));
    }
}
