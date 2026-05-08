use super::types::Message;
use super::{CompletionRequest, LlmProvider};

/// Rough token estimate: ~4 chars per token for English text.
/// Intentionally conservative (over-estimates) so we compact before hitting the wall.
fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32 + 3) / 4
}

/// Estimate the token cost of a single message.
fn message_tokens(msg: &Message) -> u32 {
    match msg {
        Message::System(text) | Message::User(text) => estimate_tokens(text),
        Message::Assistant { text, tool_calls, .. } => {
            let mut t = estimate_tokens(text);
            for tc in tool_calls {
                t += estimate_tokens(tc.name.as_str());
                t += estimate_tokens(&tc.arguments.to_string());
            }
            t
        }
        Message::ToolResult(r) => estimate_tokens(&r.content) + estimate_tokens(r.call_id.as_str()),
        // Rough heuristic — vision tokens vary by provider (Anthropic ~1500,
        // OpenAI depends on resolution). 1000/image is a conservative estimate.
        Message::Image { items } => items.len() as u32 * 1000,
        Message::SubAgentReply { agent_id, text } => {
            estimate_tokens(agent_id) + estimate_tokens(text)
        }
        Message::SubAgentFailure { agent_id, reason } => {
            estimate_tokens(agent_id) + estimate_tokens(reason)
        }
    }
}

/// Estimate total tokens for a slice of messages.
pub fn estimate_total(messages: &[Message]) -> u32 {
    messages.iter().map(|m| message_tokens(m)).sum()
}

/// The minimum number of recent messages we always keep verbatim.
/// This ensures the model always sees enough recent context to continue coherently.
const MIN_KEEP_RECENT: usize = 6;

/// Fraction of context_limit at which we trigger compaction.
/// Leaves headroom for the model's response and tool definitions.
const COMPACT_THRESHOLD: f32 = 0.75;

/// Compact messages if they exceed the context limit.
///
/// Returns a new message list where the oldest messages (beyond the most recent
/// `MIN_KEEP_RECENT`) are replaced with a single summary system message.
///
/// The system prompt (first message if `Message::System`) is always preserved.
///
/// Returns `None` if no compaction was needed (under threshold).
pub async fn compact_if_needed(
    messages: &[Message],
    context_limit: u32,
    provider: &dyn LlmProvider,
    model: &str,
) -> Option<Vec<Message>> {
    let total = estimate_total(messages);
    let threshold = (context_limit as f32 * COMPACT_THRESHOLD) as u32;

    if total <= threshold {
        return None;
    }

    // Determine which messages to keep vs. summarize.
    // Always preserve: system prompt (index 0 if System) + last MIN_KEEP_RECENT messages.
    let has_system = matches!(messages.first(), Some(Message::System(_)));
    let system_count = if has_system { 1 } else { 0 };
    let non_system = &messages[system_count..];

    if non_system.len() <= MIN_KEEP_RECENT {
        // Not enough messages to compact — keep everything.
        return None;
    }

    let split = non_system.len() - MIN_KEEP_RECENT;
    let to_summarize = &non_system[..split];
    let to_keep = &non_system[split..];

    // Build a summary request — ask the LLM to condense the old messages.
    let summary_prompt = build_summary_prompt(to_summarize);

    let summary = match request_summary(provider, model, &summary_prompt).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("context compaction failed, sending full history: {e}");
            return None;
        }
    };

    // Assemble the compacted message list.
    let mut compacted = Vec::with_capacity(system_count + 1 + to_keep.len());

    if has_system {
        compacted.push(messages[0].clone());
    }

    compacted.push(Message::System(format!(
        "[Summary of earlier conversation]\n{summary}"
    )));
    compacted.extend(to_keep.iter().cloned());

    log::info!(
        "compacted context: {total} est. tokens → {} est. tokens ({} messages summarized)",
        estimate_total(&compacted),
        to_summarize.len(),
    );

    Some(compacted)
}

/// Summarize an entire message history unconditionally (used by manual
/// compaction). Unlike `compact_if_needed`, this does not gate on token
/// thresholds and does not keep any messages verbatim — the returned string
/// is a single summary covering every message in `messages`.
pub async fn summarize_all(
    messages: &[Message],
    provider: &dyn LlmProvider,
    model: &str,
) -> Result<String, super::LlmError> {
    let prompt = build_summary_prompt(messages);
    request_summary(provider, model, &prompt).await
}

/// Format messages into a text block for the summarization request.
fn build_summary_prompt(messages: &[Message]) -> String {
    let mut buf = String::with_capacity(4096);
    buf.push_str(
        "Summarize the following conversation excerpt. \
         Preserve all key facts, decisions, user requests, and outcomes. \
         Be concise but complete — this summary replaces the original messages.\n\n",
    );

    for msg in messages {
        match msg {
            Message::System(text) => {
                buf.push_str("[System]: ");
                buf.push_str(text);
            }
            Message::User(text) => {
                buf.push_str("[User]: ");
                buf.push_str(text);
            }
            Message::Assistant { text, tool_calls, .. } => {
                buf.push_str("[Assistant]: ");
                buf.push_str(text);
                for tc in tool_calls {
                    buf.push_str(&format!("\n  [Tool call: {}]", tc.name));
                }
            }
            Message::ToolResult(r) => {
                let status = if r.is_error { "error" } else { "ok" };
                // Truncate very large tool results in the summary input.
                let content = if r.content.len() > 500 {
                    let end = r.content.floor_char_boundary(500);
                    format!("{}... (truncated)", &r.content[..end])
                } else {
                    r.content.clone()
                };
                buf.push_str(&format!("[Tool result ({status})]: {content}"));
            }
            Message::Image { items } => {
                buf.push_str(&format!("[{} image(s) attached]", items.len()));
            }
            Message::SubAgentReply { agent_id, text } => {
                buf.push_str(&format!("[{agent_id} response]: {text}"));
            }
            Message::SubAgentFailure { agent_id, reason } => {
                buf.push_str(&format!("[{agent_id} failed]: {reason}"));
            }
        }
        buf.push('\n');
    }

    buf
}

/// Make a cheap, non-streaming LLM call to get a summary.
async fn request_summary(
    provider: &dyn LlmProvider,
    model: &str,
    prompt: &str,
) -> Result<String, super::LlmError> {
    let request = CompletionRequest {
        model: model.into(),
        messages: vec![Message::User(prompt.to_string())],
        tools: Vec::new(),
        temperature: Some(0.0),
        presence_penalty: None,
        max_tokens: Some(1024),
        thinking_enabled: false,
        extensions: Default::default(),
    };

    let response = provider.complete(request).await?;
    Ok(response.text)
}
