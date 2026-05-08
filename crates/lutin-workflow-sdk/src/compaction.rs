//! LLM-driven transcript compaction for long-running agent sessions.
//!
//! Compaction kicks in when an agent's transcript grows past a
//! persona-configured threshold: the older prefix is summarised by a
//! one-shot LLM call and replaced with a single `Message::Summary`.
//! The full pre-compaction prefix is returned to the caller so each
//! workflow can archive it however it likes (sidecar file, blob store,
//! etc.) — this module deliberately does no IO.
//!
//! Reusable across workflows: the chat workflow drives this pre-turn,
//! but any workflow that owns an [`lutin_agent_sdk::Agent`] can call
//! [`maybe_compact`] with the same shape.

use lutin_agent_sdk::Agent;
use lutin_entities::Persona;
use lutin_llm::{
    CompletionRequest, LlmError, LlmProvider, Message, ModelId, ToolCall, ToolResultContent,
};
use thiserror::Error;

/// Per-call compaction parameters resolved from a [`Persona`].
///
/// Build once per turn via [`CompactionConfig::from_persona`]. `None`
/// means compaction is disabled for that persona — no further work to
/// do.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Compact when the agent's owned transcript reaches this length.
    pub threshold_messages: u32,
    /// How many recent user turns to keep verbatim. Older messages are
    /// folded into the summary.
    pub keep_recent_user_turns: u32,
}

impl CompactionConfig {
    /// Read compaction settings off a [`Persona`]. Returns `None` when
    /// the persona has no `compaction_threshold_messages` — i.e. the
    /// persona has not opted in.
    pub fn from_persona(persona: &Persona) -> Option<Self> {
        let threshold_messages = persona.compaction_threshold_messages?;
        if threshold_messages == 0 {
            return None;
        }
        let keep_recent_user_turns = persona
            .compaction_keep_recent_user_turns
            .unwrap_or_else(|| (threshold_messages / 2).max(1));
        Some(Self {
            threshold_messages,
            keep_recent_user_turns,
        })
    }
}

/// What [`maybe_compact`] did when it ran.
///
/// `archived_prefix` is the slice of the agent's transcript that was
/// removed and replaced by the summary — workflows persist this if
/// they want users to be able to inspect dropped context later.
#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    pub summary: String,
    /// Messages that were removed from the agent in compaction order
    /// (oldest first). Excludes any leading `Message::System` that was
    /// preserved at the front of the transcript.
    pub archived_prefix: Vec<Message>,
    /// Index in the pre-compaction transcript where the archived prefix
    /// began (and where the new `Message::Summary` now sits). Workflows
    /// that mirror the agent's messages into a richer per-message
    /// store use this to splice their own structures consistently.
    pub summarize_range_start: usize,
    /// Number of messages kept (excluding the new `Message::Summary`
    /// and any preserved leading System messages).
    pub kept: usize,
}

#[derive(Debug, Error)]
pub enum CompactionError {
    #[error("agent busy — cannot compact while a turn is in flight")]
    AgentBusy,
    #[error("provider: {0}")]
    Provider(#[from] LlmError),
}

/// If the persona enables compaction and the agent's transcript has
/// crossed the threshold, summarise the older prefix and splice in a
/// `Message::Summary` in its place. The agent's owned transcript is
/// mutated in-place via [`Agent::edit_messages`].
///
/// Returns `Ok(None)` when:
/// - compaction is disabled,
/// - the transcript is below the threshold,
/// - or there are too few user turns to safely cut without orphaning
///   tool exchanges.
///
/// `provider` and `model` are used for the one-shot summary call —
/// callers reuse whatever provider they already built for the persona.
pub async fn maybe_compact(
    agent: &mut Agent,
    provider: &dyn LlmProvider,
    model: &ModelId,
    cfg: &CompactionConfig,
) -> Result<Option<CompactionOutcome>, CompactionError> {
    let messages = agent.messages();
    if (messages.len() as u32) < cfg.threshold_messages {
        return Ok(None);
    }

    let plan = match plan_compaction(messages, cfg.keep_recent_user_turns as usize) {
        Some(p) => p,
        None => return Ok(None),
    };

    let range = plan.summarize_range;
    let to_summarize: Vec<Message> = messages[range.clone()].to_vec();
    if to_summarize.is_empty() {
        return Ok(None);
    }

    let prompt = build_summary_prompt(&to_summarize);
    let summary = request_summary(provider, model, &prompt).await?;

    let summarize_range_start = range.start;
    let kept = messages.len() - to_summarize.len();
    let summary_msg = Message::Summary { text: summary.clone() };

    agent
        .edit_messages(|v| {
            v.splice(range, std::iter::once(summary_msg));
        })
        .map_err(|_| CompactionError::AgentBusy)?;

    // `to_summarize` is built solely for the prompt + this archive — move,
    // don't clone, now that the prompt has been consumed.
    Ok(Some(CompactionOutcome {
        summary,
        archived_prefix: to_summarize,
        summarize_range_start,
        kept,
    }))
}

#[derive(Debug, Clone)]
struct CompactionPlan {
    /// `messages[summarize_range]` is the contiguous slice to fold into
    /// the summary. Always begins after any leading System messages and
    /// ends at a User-message boundary so we never split an
    /// Assistant{tool_calls} ↔ ToolResult pair.
    summarize_range: std::ops::Range<usize>,
}

/// Decide which slice of the transcript to summarise. Returns `None`
/// when the transcript has fewer than `keep_recent_user_turns + 1`
/// user messages — i.e. there is nothing to cut without orphaning the
/// recent context.
fn plan_compaction(messages: &[Message], keep_recent_user_turns: usize) -> Option<CompactionPlan> {
    if keep_recent_user_turns == 0 {
        return None;
    }

    let leading_system_end = messages
        .iter()
        .position(|m| !matches!(m, Message::System(_)))
        .unwrap_or(messages.len());

    let mut count = 0usize;
    let mut keep_from: Option<usize> = None;
    for (i, m) in messages.iter().enumerate().rev() {
        if matches!(m, Message::User(_)) {
            count += 1;
            if count == keep_recent_user_turns {
                keep_from = Some(i);
                break;
            }
        }
    }

    let keep_from = keep_from?;
    if keep_from <= leading_system_end {
        return None;
    }

    Some(CompactionPlan {
        summarize_range: leading_system_end..keep_from,
    })
}

fn build_summary_prompt(messages: &[Message]) -> String {
    use std::fmt::Write;
    let mut buf = String::with_capacity(4096);
    buf.push_str(
        "Summarise the following conversation excerpt. Preserve all key \
         facts, decisions, user requests, tool outcomes, and unresolved \
         questions. Be concise but complete — this summary replaces the \
         original messages so anything you omit is gone.\n\n",
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
            Message::Assistant {
                text, tool_calls, ..
            } => {
                buf.push_str("[Assistant]: ");
                buf.push_str(text);
                for tc in tool_calls {
                    let ToolCall { name, .. } = tc;
                    let _ = write!(buf, "\n  [Tool call: {name}]");
                }
            }
            Message::ToolResult(ToolResultContent {
                content, is_error, ..
            }) => {
                let status = if *is_error { "error" } else { "ok" };
                let snippet = truncate_for_summary(content, 500);
                let _ = write!(buf, "[Tool result ({status})]: {snippet}");
            }
            Message::Image { items } => {
                let _ = write!(buf, "[{} image(s) attached]", items.len());
            }
            Message::SubAgentReply { agent_id, text } => {
                let _ = write!(buf, "[{agent_id} response]: {text}");
            }
            Message::SubAgentFailure { agent_id, reason } => {
                let _ = write!(buf, "[{agent_id} failed]: {reason}");
            }
            Message::Summary { text } => {
                // Re-compaction: a previous summary is being folded into a new one.
                buf.push_str("[Earlier summary]: ");
                buf.push_str(text);
            }
        }
        buf.push('\n');
    }
    buf
}

fn truncate_for_summary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Walk back to the nearest UTF-8 boundary so we never split a multi-byte char.
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &s[..end])
}

async fn request_summary(
    provider: &dyn LlmProvider,
    model: &ModelId,
    prompt: &str,
) -> Result<String, LlmError> {
    let request = CompletionRequest {
        model: model.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn user(s: &str) -> Message {
        Message::User(s.into())
    }
    fn assistant(s: &str) -> Message {
        Message::Assistant {
            text: s.into(),
            tool_calls: Vec::new(),
            thinking: None,
        }
    }
    fn system(s: &str) -> Message {
        Message::System(s.into())
    }

    #[test]
    fn plan_skips_when_too_few_user_turns() {
        let msgs = vec![user("u1"), assistant("a1"), user("u2"), assistant("a2")];
        // keep 2 recent user turns — both u1 and u2 are recent, nothing to cut.
        assert!(plan_compaction(&msgs, 2).is_none());
    }

    #[test]
    fn plan_cuts_at_user_boundary() {
        let msgs = vec![
            user("u1"),
            assistant("a1"),
            user("u2"),
            assistant("a2"),
            user("u3"),
            assistant("a3"),
        ];
        let plan = plan_compaction(&msgs, 1).expect("should plan");
        // keep last 1 user turn (u3 onward); summarise [u1..u3) = indices 0..4.
        assert_eq!(plan.summarize_range, 0..4);
    }

    #[test]
    fn plan_preserves_leading_system() {
        let msgs = vec![
            system("sys"),
            user("u1"),
            assistant("a1"),
            user("u2"),
            assistant("a2"),
            user("u3"),
            assistant("a3"),
        ];
        let plan = plan_compaction(&msgs, 1).expect("should plan");
        // leading System at 0 stays put; summarise [u1..u3) = indices 1..5.
        assert_eq!(plan.summarize_range, 1..5);
    }

    #[test]
    fn plan_zero_keep_returns_none() {
        let msgs = vec![user("u1"), assistant("a1"), user("u2")];
        assert!(plan_compaction(&msgs, 0).is_none());
    }
}
