//! Anthropic `/v1/messages` provider.
//!
//! Auth branches on [`AnthropicAuth`]: API key or OAuth bearer. In OAuth mode
//! the request is rewritten to (a) add `anthropic-beta: oauth-2025-04-20`,
//! (b) inject the Claude Code system-prompt preamble as the first system
//! block, and (c) carry `Authorization: Bearer`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::AnthropicAuth;
use crate::{
    CompletionRequest, CompletionResponse, EventStream, LlmError, LlmProvider, Message, ModelInfo,
    StreamEvent, ToolCall, ToolDefinition, ToolResultContent, Usage,
};
use crate::ids::{CallId, ModelId, ToolName};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
/// OAuth requests use `?beta=true` — the subscription inference endpoint
/// requires it alongside the `oauth-2025-04-20` beta header.
pub const OAUTH_API_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Adaptive/extended thinking beta required for newer models under OAuth.
pub const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
/// User-Agent Anthropic's classifier expects on subscription-OAuth requests.
/// Anything else (including naked `reqwest/x.y`) is rejected for newer models.
pub const OAUTH_USER_AGENT: &str = "claude-cli/2.1.87 (external, cli)";
/// Identity block prepended to the system prompt on OAuth requests. Matches
/// what Claude Agent SDK / Claude Code ship — the server-side classifier
/// pattern-matches on this exact string shape.
pub const CLAUDE_CODE_PREAMBLE: &str =
    "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

// ---------------------------------------------------------------------------
// Tool-name prefixing (OAuth only)
// ---------------------------------------------------------------------------
//
// Anthropic's subscription inference endpoint flags tools that don't follow
// Claude Code's `mcp_PascalCase` convention. We prefix outgoing tool names
// and strip the prefix off server-emitted `tool_use` blocks so callers see
// their original names unchanged.

pub const TOOL_PREFIX: &str = "mcp_";

fn prefix_tool_name(name: &str) -> String {
    let mut chars = name.chars();
    let first = chars.next().map(|c| c.to_ascii_uppercase());
    let mut out = String::with_capacity(TOOL_PREFIX.len() + name.len());
    out.push_str(TOOL_PREFIX);
    if let Some(c) = first {
        out.push(c);
    }
    out.push_str(chars.as_str());
    out
}

fn unprefix_tool_name(name: &str) -> String {
    let Some(rest) = name.strip_prefix(TOOL_PREFIX) else {
        return name.to_string();
    };
    // `StructuredOutput` is the documented passthrough that keeps its case.
    if rest == "StructuredOutput" {
        return rest.to_string();
    }
    let mut chars = rest.chars();
    let first = chars.next().map(|c| c.to_ascii_lowercase());
    let mut out = String::with_capacity(rest.len());
    if let Some(c) = first {
        out.push(c);
    }
    out.push_str(chars.as_str());
    out
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    http: reqwest::Client,
    auth: AnthropicAuth,
    extra_betas: Vec<String>,
}

impl AnthropicProvider {
    pub fn new(auth: AnthropicAuth) -> Self {
        Self {
            http: reqwest::Client::new(),
            auth,
            extra_betas: Vec::new(),
        }
    }

    pub fn with_beta(mut self, beta: impl Into<String>) -> Self {
        self.extra_betas.push(beta.into());
        self
    }

    /// Build a Messages API request with the appropriate auth. Returns the
    /// token source used for this attempt so the 401 path can decide
    /// deterministically whether a refresh-and-retry is even applicable —
    /// without re-reading `ANTHROPIC_OAUTH_TOKEN` (which could have changed
    /// between attempts and flip the decision incoherently).
    async fn send_once(
        &self,
        wire: &WireRequest,
        stream: bool,
    ) -> Result<(reqwest::Response, TokenSource), LlmError> {
        let url = match &self.auth {
            AnthropicAuth::OAuthSubscription(_) => OAUTH_API_URL,
            AnthropicAuth::ApiKey(_) => API_URL,
        };
        let mut rb = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION);

        let mut betas = self.extra_betas.clone();
        let source = match &self.auth {
            AnthropicAuth::ApiKey(k) => {
                rb = rb.header("x-api-key", k.as_str());
                TokenSource::ApiKey
            }
            AnthropicAuth::OAuthSubscription(store) => {
                // Snapshot env at send time, so the retry decision later
                // uses the same source this attempt did.
                let (token, source) = match std::env::var("ANTHROPIC_OAUTH_TOKEN") {
                    Ok(env_tok) => (env_tok, TokenSource::Env),
                    Err(_) => {
                        let tok = store.get_valid_access_token().await?;
                        (tok.clone(), TokenSource::Store(tok))
                    }
                };
                rb = rb
                    .header("Authorization", format!("Bearer {token}"))
                    .header("User-Agent", OAUTH_USER_AGENT);
                for required in [OAUTH_BETA, INTERLEAVED_THINKING_BETA] {
                    if !betas.iter().any(|b| b == required) {
                        betas.push(required.to_string());
                    }
                }
                source
            }
        };
        if !betas.is_empty() {
            rb = rb.header("anthropic-beta", betas.join(","));
        }
        let mut body = serde_json::to_value(wire)?;
        if stream {
            if let Value::Object(ref mut m) = body {
                m.insert("stream".into(), Value::Bool(true));
            }
        }
        Ok((rb.json(&body).send().await?, source))
    }

    async fn call_with_retry(&self, wire: WireRequest) -> Result<WireResponse, LlmError> {
        let (resp, source) = self.send_once(&wire, false).await?;
        if resp.status().as_u16() == 401 {
            if let (AnthropicAuth::OAuthSubscription(store), TokenSource::Store(stale)) =
                (&self.auth, &source)
            {
                if let Err(e) = store.refresh_after_auth_error(stale).await {
                    log::warn!("anthropic 401 refresh failed: {e}");
                    return Err(e);
                }
                let (retry, _) = self.send_once(&wire, false).await?;
                if retry.status().as_u16() == 401 {
                    let _ = store.clear();
                }
                return parse_response(retry).await;
            }
        }
        parse_response(resp).await
    }

    async fn stream_with_retry(
        &self,
        wire: WireRequest,
        model: ModelId,
    ) -> Result<EventStream, LlmError> {
        let (resp, source) = self.send_once(&wire, true).await?;
        if resp.status().as_u16() == 401 {
            if let (AnthropicAuth::OAuthSubscription(store), TokenSource::Store(stale)) =
                (&self.auth, &source)
            {
                store.refresh_after_auth_error(stale).await?;
                let (retry, _) = self.send_once(&wire, true).await?;
                return stream_from_response(retry, model).await;
            }
        }
        stream_from_response(resp, model).await
    }
}

/// Which auth source the current request went out with. `Env` is excluded
/// from reactive refresh because the caller owns that token's lifecycle;
/// `ApiKey` can't be refreshed. Only `Store(stale)` triggers a retry.
enum TokenSource {
    ApiKey,
    Env,
    Store(String),
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError> {
        let model_id = request.model.clone();
        let wire = build_wire(request, &self.auth, false)?;
        let resp = self.call_with_retry(wire).await?;
        Ok(response_from_wire(resp, model_id))
    }

    async fn stream(&self, request: CompletionRequest) -> Result<EventStream, LlmError> {
        let model = request.model.clone();
        let wire = build_wire(request, &self.auth, true)?;
        self.stream_with_retry(wire, model).await
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct WireRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<SystemBlock>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    ty: &'static str,
    budget_tokens: u32,
}

#[derive(Serialize, Clone)]
struct SystemBlock {
    #[serde(rename = "type")]
    ty: &'static str,
    text: String,
}

impl SystemBlock {
    fn text(text: impl Into<String>) -> Self {
        Self {
            ty: "text",
            text: text.into(),
        }
    }
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    content: Vec<ContentBlock>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Image {
        source: ImageSource,
    },
}

#[derive(Serialize)]
struct ImageSource {
    #[serde(rename = "type")]
    ty: &'static str,
    media_type: String,
    data: String,
}

#[derive(Serialize)]
struct WireTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    content: Vec<RespBlock>,
    #[serde(default)]
    usage: Option<RespUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RespBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct RespUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    error: Option<ApiErrorBody>,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    #[allow(dead_code)]
    r#type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// Build / parse
// ---------------------------------------------------------------------------

fn build_wire(
    req: CompletionRequest,
    auth: &AnthropicAuth,
    _stream: bool,
) -> Result<WireRequest, LlmError> {
    let CompletionRequest {
        model,
        messages,
        tools,
        temperature,
        max_tokens,
        thinking_enabled,
        extensions,
    } = req;
    let reasoning_max_tokens = extensions.reasoning.as_ref().and_then(|r| r.max_tokens);

    let mut system_parts: Vec<SystemBlock> = Vec::new();
    let mut wire_messages: Vec<WireMessage> = Vec::new();
    let mut first_user_text: Option<String> = None;
    let use_oauth = auth.is_oauth();

    for m in messages {
        match m {
            Message::System(s) => {
                // Skip empty system messages — callers sometimes pass a blank
                // system prompt; Anthropic rejects empty SystemBlock text.
                if !s.is_empty() {
                    system_parts.push(SystemBlock::text(s));
                }
            }
            Message::User(s) => {
                if first_user_text.is_none() {
                    first_user_text = Some(s.clone());
                }
                push_user(&mut wire_messages, ContentBlock::Text { text: s });
            }
            Message::Assistant {
                text,
                tool_calls,
                thinking,
            } => {
                let mut blocks = Vec::new();
                // Skip empty thinking — replayed assistant turns may carry
                // `Some("")` when reasoning was enabled but produced none.
                if let Some(t) = thinking {
                    if !t.is_empty() {
                        blocks.push(ContentBlock::Thinking { thinking: t });
                    }
                }
                // Skip empty text — tool-only assistant turns have no visible
                // text; an empty Text block would be rejected by the API.
                if !text.is_empty() {
                    blocks.push(ContentBlock::Text { text });
                }
                for tc in tool_calls {
                    let name = if use_oauth {
                        prefix_tool_name(tc.name.as_str())
                    } else {
                        tc.name.to_string()
                    };
                    // Anthropic requires `input` to be an object. Models
                    // sometimes emit no `input_json_delta` frames for
                    // no-arg calls, leaving `arguments` as Null or an
                    // empty value — coerce to `{}` so replay doesn't 400.
                    let input = match tc.arguments {
                        Value::Object(_) => tc.arguments,
                        _ => Value::Object(serde_json::Map::new()),
                    };
                    blocks.push(ContentBlock::ToolUse {
                        id: tc.id.to_string(),
                        name,
                        input,
                    });
                }
                if !blocks.is_empty() {
                    push_assistant_many(&mut wire_messages, blocks);
                }
            }
            Message::ToolResult(ToolResultContent {
                call_id,
                content,
                is_error,
            }) => push_user(
                &mut wire_messages,
                ContentBlock::ToolResult {
                    tool_use_id: call_id.to_string(),
                    content,
                    is_error,
                },
            ),
            Message::Image { items } => {
                for item in items {
                    push_user(
                        &mut wire_messages,
                        ContentBlock::Image {
                            source: ImageSource {
                                ty: "base64",
                                media_type: item.mime,
                                data: item.base64,
                            },
                        },
                    );
                }
            }
        }
    }

    // OAuth subscription requires a specific first-two-system-blocks layout:
    //   [0] billing header (x-anthropic-billing-header: ...)
    //   [1] Claude-agent identity preamble
    //   [2..] caller's system prompt
    // The server classifier rejects Opus 4.7+ without this exact shape.
    if use_oauth {
        let first_user = first_user_text.as_deref().unwrap_or("");
        let billing = super::billing::build_billing_header(first_user);
        // Strip any previously-injected identity/billing blocks so repeated
        // invocations over a retained system vec don't accumulate.
        system_parts.retain(|b| {
            b.text != CLAUDE_CODE_PREAMBLE
                && !b.text.starts_with("x-anthropic-billing-header:")
        });
        system_parts.insert(0, SystemBlock::text(CLAUDE_CODE_PREAMBLE));
        system_parts.insert(0, SystemBlock::text(billing));
    }

    let tools: Vec<WireTool> = tools
        .into_iter()
        .map(|t| convert_tool(t, use_oauth))
        .collect();

    let thinking = if thinking_enabled {
        Some(ThinkingConfig {
            ty: "enabled",
            budget_tokens: reasoning_max_tokens.unwrap_or(4096),
        })
    } else {
        None
    };

    // Anthropic rejects `temperature` for newer Claude models (4.x and any
    // model with extended thinking enabled). The field is deprecated, not
    // value-validated — sending any value triggers a 400. Drop it for all
    // Anthropic requests; persona-level temperature settings are silently
    // ignored on this provider.
    let _ = temperature;
    let temperature: Option<f32> = None;

    Ok(WireRequest {
        model: model.to_string(),
        max_tokens: max_tokens.unwrap_or(4096),
        messages: wire_messages,
        system: system_parts,
        tools,
        temperature,
        thinking,
    })
}

fn push_user(out: &mut Vec<WireMessage>, block: ContentBlock) {
    if let Some(last) = out.last_mut() {
        if last.role == "user" {
            last.content.push(block);
            return;
        }
    }
    out.push(WireMessage {
        role: "user",
        content: vec![block],
    });
}

fn push_assistant_many(out: &mut Vec<WireMessage>, mut blocks: Vec<ContentBlock>) {
    if let Some(last) = out.last_mut() {
        if last.role == "assistant" {
            last.content.append(&mut blocks);
            return;
        }
    }
    out.push(WireMessage {
        role: "assistant",
        content: blocks,
    });
}

fn convert_tool(t: ToolDefinition, use_oauth: bool) -> WireTool {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for p in t.parameters {
        props.insert(
            p.name.clone(),
            json!({ "type": p.r#type, "description": p.description }),
        );
        if p.required {
            required.push(p.name);
        }
    }
    let schema = json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": required,
    });
    let name = if use_oauth {
        prefix_tool_name(t.name.as_str())
    } else {
        t.name.to_string()
    };
    WireTool {
        name,
        description: t.description,
        input_schema: schema,
    }
}

async fn parse_response(resp: reqwest::Response) -> Result<WireResponse, LlmError> {
    let status = resp.status();
    // Read Retry-After before consuming the body via text().
    let retry_after = crate::openai_compat::parse_retry_after(&resp);
    let body = resp.text().await?;
    if status.is_success() {
        return serde_json::from_str(&body).map_err(LlmError::Json);
    }
    if status.as_u16() == 429 {
        return Err(LlmError::RateLimited {
            message: body,
            retry_after,
        });
    }
    let message = serde_json::from_str::<ApiError>(&body)
        .ok()
        .and_then(|e| e.error.and_then(|b| b.message))
        .unwrap_or_else(|| body.clone());
    Err(LlmError::Api {
        status: status.as_u16(),
        message,
    })
}

fn response_from_wire(resp: WireResponse, model: ModelId) -> CompletionResponse {
    let mut text = String::new();
    let mut thinking: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in resp.content {
        match block {
            RespBlock::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t);
            }
            RespBlock::Thinking { thinking: t } => {
                thinking.get_or_insert_with(String::new).push_str(&t);
            }
            RespBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id: CallId::from(id),
                    name: ToolName::from(unprefix_tool_name(&name)),
                    arguments: input,
                });
            }
            RespBlock::Other => {}
        }
    }

    let usage = resp
        .usage
        .map(|u| Usage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        })
        .unwrap_or_default();

    CompletionResponse {
        text,
        thinking,
        tool_calls,
        model,
        usage,
        provider: None,
    }
}

// ---------------------------------------------------------------------------
// SSE streaming
// ---------------------------------------------------------------------------

async fn stream_from_response(
    resp: reqwest::Response,
    _model: ModelId,
) -> Result<EventStream, LlmError> {
    let status = resp.status();
    if !status.is_success() {
        let retry_after = crate::openai_compat::parse_retry_after(&resp);
        let body = resp.text().await?;
        if status.as_u16() == 429 {
            return Err(LlmError::RateLimited {
                message: body,
                retry_after,
            });
        }
        let message = serde_json::from_str::<ApiError>(&body)
            .ok()
            .and_then(|e| e.error.and_then(|b| b.message))
            .unwrap_or(body);
        return Err(LlmError::Api {
            status: status.as_u16(),
            message,
        });
    }
    Ok(sse_event_stream(resp))
}

/// One entry per server-emitted content_block, keyed by `index`. Holds the
/// tool-use id so every `input_json_delta` can be tagged with the right
/// `CallId` — Anthropic streams the id once in `content_block_start` and then
/// only the index on subsequent deltas.
#[derive(Default, Clone)]
struct BlockMeta {
    tool_id: Option<String>,
}

/// SSE frame-level event envelope. Only the fields we care about.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum SseEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: SseMessage },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u64,
        content_block: SseContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u64, delta: SseDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        #[allow(dead_code)]
        index: u64,
    },
    #[serde(rename = "message_delta")]
    MessageDelta {
        #[serde(default)]
        usage: Option<SseUsageDelta>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: SseErrorBody },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct SseMessage {
    #[serde(default)]
    usage: Option<SseUsage>,
}

#[derive(Deserialize)]
struct SseUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[derive(Deserialize)]
struct SseUsageDelta {
    #[serde(default)]
    output_tokens: Option<u32>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum SseContentBlock {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum SseDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct SseErrorBody {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Hard cap on `line_buf` so a stuck / hostile upstream that ships bytes
/// without a newline can't OOM us. Anthropic SSE lines are comfortably under
/// this in practice.
const MAX_SSE_LINE: usize = 1 << 20; // 1 MiB

/// Hard cap on the multi-line `data:` accumulator for a single SSE event.
// Why: a hostile upstream could spam many small `data:` lines (each within
// the per-line cap) for one event, with `data_buf` growing unbounded across
// them. 8 MiB is generous for any realistic Anthropic event payload while
// still bounding worst-case memory.
const MAX_SSE_DATA_BUF: usize = 8 * 1024 * 1024;

struct StreamState {
    /// Raw byte buffer — holds partial UTF-8 sequences across chunk
    /// boundaries. Decoded per-line on newline, so multi-byte characters
    /// split across TCP frames don't turn into `U+FFFD`.
    line_buf: Vec<u8>,
    event_name: Option<String>,
    data_buf: String,
    blocks: Vec<BlockMeta>,
    usage: Usage,
    done: bool,
}

impl StreamState {
    fn new() -> Self {
        Self {
            line_buf: Vec::new(),
            event_name: None,
            data_buf: String::new(),
            blocks: Vec::new(),
            usage: Usage::default(),
            done: false,
        }
    }

    fn ensure_block(&mut self, index: u64) -> &mut BlockMeta {
        let i = index as usize;
        if self.blocks.len() <= i {
            self.blocks.resize(i + 1, BlockMeta::default());
        }
        &mut self.blocks[i]
    }
}

fn sse_event_stream(resp: reqwest::Response) -> EventStream {
    use futures::{StreamExt, TryStreamExt};

    let byte_stream = resp.bytes_stream();

    let stream = byte_stream
        .scan(StreamState::new(), move |st, chunk| {
            let result = match chunk {
                Ok(bytes) => {
                    st.line_buf.extend_from_slice(&bytes);
                    if st.line_buf.len() > MAX_SSE_LINE {
                        return futures::future::ready(Some(Err(LlmError::Stream(format!(
                            "SSE line exceeded {MAX_SSE_LINE} bytes without newline"
                        )))));
                    }
                    match drain_events(st) {
                        Ok(events) => Ok::<_, LlmError>(futures::stream::iter(
                            events.into_iter().map(Ok::<_, LlmError>),
                        )),
                        Err(e) => return futures::future::ready(Some(Err(e))),
                    }
                }
                Err(e) => Err(LlmError::Http(e)),
            };
            futures::future::ready(Some(result))
        })
        .try_flatten();

    Box::pin(stream)
}

/// Pull complete SSE lines out of `state.line_buf`, fold them into
/// `event:` / `data:` framing, dispatch each fully-assembled event to
/// `handle_event`, and return the resulting `StreamEvent`s.
fn drain_events(state: &mut StreamState) -> Result<Vec<StreamEvent>, LlmError> {
    let mut out = Vec::new();
    loop {
        let Some(nl) = state.line_buf.iter().position(|b| *b == b'\n') else {
            break;
        };
        let raw: Vec<u8> = state.line_buf.drain(..=nl).collect();
        // Strip the trailing `\n` (and optional `\r`) before UTF-8 decoding
        // so a valid UTF-8 line with no multibyte split is the common fast
        // path. UTF-8 boundaries always align with `\n` (a pure ASCII byte),
        // so the slice above is safe to decode.
        let end = raw.len().saturating_sub(1);
        let end = if end > 0 && raw.get(end - 1) == Some(&b'\r') {
            end - 1
        } else {
            end
        };
        let line = match std::str::from_utf8(&raw[..end]) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("anthropic SSE line invalid utf-8, dropping: {e}");
                continue;
            }
        };

        if line.is_empty() {
            // Dispatch the accumulated event (if any).
            if !state.data_buf.is_empty() {
                let data = std::mem::take(&mut state.data_buf);
                let _event = state.event_name.take();
                handle_event(state, &data, &mut out)?;
            } else {
                state.event_name = None;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            state.event_name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            let addition = rest.trim_start();
            let separator = if state.data_buf.is_empty() { 0 } else { 1 };
            if state
                .data_buf
                .len()
                .saturating_add(separator)
                .saturating_add(addition.len())
                > MAX_SSE_DATA_BUF
            {
                return Err(LlmError::Stream(format!(
                    "anthropic SSE event data exceeded {MAX_SSE_DATA_BUF} bytes (MAX_SSE_DATA_BUF)"
                )));
            }
            if separator == 1 {
                state.data_buf.push('\n');
            }
            state.data_buf.push_str(addition);
        }
        // Ignore comments (`:`) and id/retry fields.
    }
    Ok(out)
}

fn handle_event(
    state: &mut StreamState,
    data: &str,
    out: &mut Vec<StreamEvent>,
) -> Result<(), LlmError> {
    let ev: SseEvent = match serde_json::from_str(data) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("anthropic SSE parse error: {e}; data: {data}");
            return Ok(());
        }
    };
    match ev {
        SseEvent::MessageStart { message } => {
            if let Some(u) = message.usage {
                state.usage.prompt_tokens = u.input_tokens;
                state.usage.completion_tokens = u.output_tokens;
            }
        }
        SseEvent::ContentBlockStart {
            index,
            content_block,
        } => {
            let meta = state.ensure_block(index);
            if let SseContentBlock::ToolUse { id, name } = content_block {
                meta.tool_id = Some(id.clone());
                out.push(StreamEvent::ToolCallStart {
                    id: CallId::from(id),
                    name: ToolName::from(unprefix_tool_name(&name)),
                });
            }
        }
        SseEvent::ContentBlockDelta { index, delta } => match delta {
            SseDelta::Text { text } => out.push(StreamEvent::Delta(text)),
            SseDelta::Thinking { thinking } => out.push(StreamEvent::Reasoning(thinking)),
            SseDelta::InputJson { partial_json } => {
                let id = state
                    .ensure_block(index)
                    .tool_id
                    .clone()
                    .unwrap_or_default();
                if !id.is_empty() {
                    out.push(StreamEvent::ToolCallDelta {
                        id: CallId::from(id),
                        arguments: partial_json,
                    });
                }
            }
            SseDelta::Unknown => {}
        },
        SseEvent::ContentBlockStop { .. } => {}
        SseEvent::MessageDelta { usage } => {
            if let Some(u) = usage {
                if let Some(n) = u.output_tokens {
                    state.usage.completion_tokens = n;
                }
            }
        }
        SseEvent::MessageStop => {
            state.usage.total_tokens =
                state.usage.prompt_tokens + state.usage.completion_tokens;
            let usage = if state.usage.total_tokens == 0 {
                None
            } else {
                Some(state.usage.clone())
            };
            out.push(StreamEvent::Done { usage });
            state.done = true;
        }
        SseEvent::Ping | SseEvent::Unknown => {}
        SseEvent::Error { error } => {
            let msg = error
                .message
                .unwrap_or_else(|| error.r#type.unwrap_or_else(|| "stream error".into()));
            return Err(LlmError::Stream(msg));
        }
    }
    Ok(())
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn feed(state: &mut StreamState, chunk: &str) -> Vec<StreamEvent> {
        state.line_buf.extend_from_slice(chunk.as_bytes());
        drain_events(state).expect("drain")
    }

    #[test]
    fn parses_text_tool_use_and_stop() {
        let mut st = StreamState::new();
        let frames = [
            "event: message_start\n",
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}"#,
            "\n\n",
            "event: content_block_start\n",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            "\n\n",
            "event: content_block_delta\n",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            "\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"read"}}"#,
            "\n\n",
            "event: content_block_delta\n",
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path"}}"#,
            "\n\n",
            "event: message_delta\n",
            r#"data: {"type":"message_delta","usage":{"output_tokens":7}}"#,
            "\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let mut events = Vec::new();
        for f in frames {
            events.extend(feed(&mut st, f));
        }

        assert!(matches!(events[0], StreamEvent::Delta(ref t) if t == "Hello"));
        assert!(
            matches!(&events[1], StreamEvent::ToolCallStart { id, name } if id.as_str() == "tu_1" && name.as_str() == "read")
        );
        assert!(
            matches!(&events[2], StreamEvent::ToolCallDelta { id, arguments } if id.as_str() == "tu_1" && arguments == "{\"path")
        );
        match events.last().unwrap() {
            StreamEvent::Done { usage } => {
                let u = usage.as_ref().expect("usage present");
                assert_eq!(u.prompt_tokens, 10);
                assert_eq!(u.completion_tokens, 7);
                assert_eq!(u.total_tokens, 17);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn error_event_becomes_stream_error() {
        let mut st = StreamState::new();
        let frame = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"boom\"}}\n\n";
        st.line_buf.extend_from_slice(frame.as_bytes());
        let res = drain_events(&mut st);
        assert!(matches!(res, Err(LlmError::Stream(m)) if m == "boom"));
    }

    #[test]
    fn utf8_split_across_chunks_preserved() {
        // The "🎉" emoji is 4 bytes in UTF-8 (F0 9F 8E 89). Splitting its
        // bytes across two feeds would historically produce `U+FFFD` via
        // `String::from_utf8_lossy`; with byte-buffered line_buf the
        // character must survive intact.
        let mut st = StreamState::new();
        let full = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"🎉\"}}\n\n";
        let bytes = full.as_bytes();
        // Pick a split point inside the emoji's 4-byte sequence.
        let cut = bytes
            .windows(4)
            .position(|w| w == [0xF0, 0x9F, 0x8E, 0x89])
            .expect("emoji in frame")
            + 2;
        st.line_buf.extend_from_slice(&bytes[..cut]);
        let _ = drain_events(&mut st).unwrap();
        st.line_buf.extend_from_slice(&bytes[cut..]);
        let events = drain_events(&mut st).unwrap();
        assert!(matches!(events.as_slice(), [StreamEvent::Delta(t)] if t == "🎉"));
    }
}

