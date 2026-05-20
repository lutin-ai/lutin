//! Pure transcript projections + summary file writers.
//!
//! Lifts the engine-private `Entry` shape across the wire boundary
//! (`HistoricalMessage`, `MessageMeta`, `ChatEvent::SummaryUpdated`)
//! and writes the dormant-session `summary.json` label. Everything
//! here is free-function and side-effect-free except `write_summary`,
//! which is a best-effort disk write.

use std::path::Path;

use lutin_agent_sdk::FinishReason as AgentFinishReason;
use lutin_entities::Persona;
use lutin_storage::Resolver;
use lutin_workflow_sdk::summary as sdk_summary;
use principled::{
    ChatEvent, FinishReason, HistoricalMessage, MessageMeta, ToolOutcome, load_state,
};
use serde::Serialize;
use tracing::warn;

use crate::store::Entry;

const SUMMARY_TITLE_CHARS: usize = 80;
const SUMMARY_PREVIEW_CHARS: usize = 160;

/// Workflow-supplied summary file CP reads at `ListSessions` time
/// to label this session in the desktop's list. Schema is shared
/// across workflows (chrome reads it identically) — keep it in sync
/// with `lutin_control_protocol::SessionSummary`. We mirror the type
/// rather than depend on the CP crate so chat keeps its lean
/// dependency footprint.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatSummary {
    title: Option<String>,
    subtitle: Option<String>,
    last_activity: Option<String>,
    preview: Option<String>,
    persona: Option<String>,
    model: Option<String>,
    total_prompt_tokens: Option<u64>,
    total_completion_tokens: Option<u64>,
    context_tokens: Option<u32>,
    message_count: Option<u32>,
}

/// Project per-entry text-token stats into the shape consumed by
/// [`lutin_workflow_sdk::summary::aggregate`]. Lifts the
/// engine-private `Entry` shape across the SDK boundary without
/// dragging the full `MessageMetrics` into the shared crate.
pub(crate) fn entry_tokens(
    entries: &[Entry],
) -> impl Iterator<Item = sdk_summary::EntryTokens> + '_ {
    entries.iter().map(|e| sdk_summary::EntryTokens {
        prompt_tokens: e.metrics.text.and_then(|t| t.prompt_tokens),
        completion_tokens: e.metrics.text.and_then(|t| t.completion_tokens),
    })
}

/// Build a `SummaryUpdated` payload from the committed transcript.
/// Aggregation logic is shared with the chat engine via
/// `lutin-workflow-sdk`; only the wire-event wrap is workflow-local.
pub(crate) fn build_summary_updated(entries: &[Entry]) -> ChatEvent {
    let s = sdk_summary::aggregate(entry_tokens(entries));
    ChatEvent::SummaryUpdated {
        context_tokens: s.context_tokens,
        total_prompt_tokens: s.total_prompt_tokens,
        total_completion_tokens: s.total_completion_tokens,
    }
}

/// Build + atomically write `<state_dir>/summary.json`. Called after
/// every turn so the dormant-session label tracks the latest state;
/// also called once at runner startup so the file exists before any
/// turns happen. Failures log a warning but never bubble — a missing
/// summary just means the chrome shows a generic fallback label, not
/// that the session is broken.
pub(crate) fn write_summary(state_dir: &Path, resolver: &Resolver, entries: &[Entry]) {
    // Best-effort enrichment: persona name from session state, model
    // resolved through the persona TOML. Both are optional; failures
    // here just leave the fields blank in the summary file.
    let session_state = load_state(state_dir).ok();
    let persona_name = session_state.as_ref().and_then(|s| s.persona.clone());
    let model_override = session_state.as_ref().and_then(|s| s.model_override.clone());
    let resolved_model = model_override.or_else(|| {
        let name = persona_name.as_deref()?;
        Persona::load(resolver, name).ok().and_then(|p| p.model)
    });
    let summary = build_summary(entries, persona_name, resolved_model);
    let payload = match serde_json::to_vec_pretty(&summary) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "encode summary.json failed");
            return;
        }
    };
    let path = state_dir.join("summary.json");
    let tmp = state_dir.join("summary.json.tmp");
    if let Err(e) = std::fs::write(&tmp, &payload) {
        warn!(error = %e, path = %tmp.display(), "write summary tmp failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        warn!(error = %e, "rename summary tmp into place failed");
    }
}

fn build_summary(
    entries: &[Entry],
    persona: Option<String>,
    model: Option<String>,
) -> ChatSummary {
    let title = entries.iter().find_map(|e| match &e.message {
        lutin_llm::Message::User(text) if !text.trim().is_empty() => {
            Some(truncate_chars(text.trim(), SUMMARY_TITLE_CHARS))
        }
        _ => None,
    });
    let preview = entries.iter().rev().find_map(|e| match &e.message {
        lutin_llm::Message::Assistant { text, .. } if !text.trim().is_empty() => {
            Some(truncate_chars(text.trim(), SUMMARY_PREVIEW_CHARS))
        }
        _ => None,
    });
    let visible = entries
        .iter()
        .filter(|e| {
            matches!(
                &e.message,
                lutin_llm::Message::User(t) if !t.is_empty(),
            ) || matches!(
                &e.message,
                lutin_llm::Message::Assistant { text, .. } if !text.is_empty(),
            )
        })
        .count();
    let subtitle = if visible == 0 {
        None
    } else if visible == 1 {
        Some("1 message".into())
    } else {
        Some(format!("{visible} messages"))
    };
    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut last_prompt: Option<u32> = None;
    for e in entries {
        if let Some(t) = e.metrics.text {
            if let Some(p) = t.prompt_tokens {
                total_prompt = total_prompt.saturating_add(p as u64);
                last_prompt = Some(p);
            }
            if let Some(c) = t.completion_tokens {
                total_completion = total_completion.saturating_add(c as u64);
            }
        }
    }
    let total_prompt_tokens = (total_prompt > 0).then_some(total_prompt);
    let total_completion_tokens = (total_completion > 0).then_some(total_completion);

    ChatSummary {
        title,
        subtitle,
        last_activity: Some(chrono::Utc::now().to_rfc3339()),
        preview,
        persona,
        model,
        total_prompt_tokens,
        total_completion_tokens,
        context_tokens: last_prompt,
        message_count: Some(visible as u32),
    }
}

/// Char-aware (not byte-aware) truncation, with a single ellipsis
/// when we cut. Avoids splitting multi-byte UTF-8 sequences.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut count = 0;
    let mut end_byte = s.len();
    for (idx, _) in s.char_indices() {
        if count == max_chars {
            end_byte = idx;
            break;
        }
        count += 1;
    }
    if end_byte < s.len() {
        let mut out = s[..end_byte].to_owned();
        out.push('…');
        out
    } else {
        s.to_owned()
    }
}

/// Project the engine's `Vec<Message>` to the wire shape, preserving
/// order. Tool calls are paired with their later `Message::ToolResult`
/// by `call_id`; `projected_slots` mirrors this iteration order.
pub(crate) fn project_history(entries: &[Entry]) -> Vec<HistoricalMessage> {
    project_messages(entries.iter().map(|e| &e.message))
}

/// Same projection rules as [`project_history`], but driven from raw
/// messages (not `Entry`s) so sub-agent transcripts — which never sit
/// in the engine's `Vec<Entry>` — can reuse the exact same widget
/// shape. Pairing of `tool_calls` to their `ToolResult` happens via a
/// linear scan keyed on `call_id`.
pub(crate) fn project_messages<'a>(
    messages: impl IntoIterator<Item = &'a lutin_llm::Message> + Clone,
) -> Vec<HistoricalMessage> {
    let mut results_by_id: Vec<(&str, &lutin_llm::ToolResultContent)> = Vec::new();
    for m in messages.clone() {
        if let lutin_llm::Message::ToolResult(tr) = m {
            results_by_id.push((tr.call_id.as_str(), tr));
        }
    }
    let mut out: Vec<HistoricalMessage> = Vec::new();
    for m in messages {
        match m {
            lutin_llm::Message::User(text) if !text.is_empty() => {
                out.push(HistoricalMessage::User(text.clone()));
            }
            lutin_llm::Message::SubAgentReply { agent_id, text } => {
                out.push(HistoricalMessage::SubAgentReply {
                    agent_id: agent_id.clone(),
                    text: text.clone(),
                });
            }
            lutin_llm::Message::SubAgentFailure { agent_id, reason } => {
                out.push(HistoricalMessage::SubAgentFailure {
                    agent_id: agent_id.clone(),
                    reason: reason.clone(),
                });
            }
            lutin_llm::Message::Summary { text } => {
                out.push(HistoricalMessage::Summary { text: text.clone() });
            }
            lutin_llm::Message::Assistant { text, thinking, tool_calls } => {
                if let Some(t) = thinking
                    && !t.is_empty()
                {
                    out.push(HistoricalMessage::Thinking(t.clone()));
                }
                if !text.is_empty() {
                    out.push(HistoricalMessage::Assistant(text.clone()));
                }
                for call in tool_calls {
                    let arguments_json = serde_json::to_string(&call.arguments)
                        .expect("serializing serde_json::Value is infallible");
                    let outcome = results_by_id
                        .iter()
                        .find(|(id, _)| *id == call.id.as_str())
                        .map(|(_, tr)| {
                            if tr.is_error {
                                ToolOutcome::Failed(tr.content.clone())
                            } else {
                                ToolOutcome::Ok(tr.content.clone())
                            }
                        });
                    out.push(HistoricalMessage::Tool {
                        call_id: call.id.as_str().to_string(),
                        name: call.name.as_str().to_string(),
                        arguments_json,
                        outcome,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Project entries to wire `MessageMeta` aligned 1:1 with `project_history`.
pub(crate) fn project_metrics(entries: &[Entry]) -> Vec<MessageMeta> {
    let mut out: Vec<MessageMeta> = Vec::with_capacity(entries.len());
    for entry in entries {
        let ts = entry.metrics.timestamp.clone();
        match &entry.message {
            lutin_llm::Message::User(text) if !text.is_empty() => {
                out.push(MessageMeta::User { timestamp: ts });
            }
            lutin_llm::Message::SubAgentReply { .. } => {
                out.push(MessageMeta::SubAgentReply { timestamp: ts });
            }
            lutin_llm::Message::SubAgentFailure { .. } => {
                out.push(MessageMeta::SubAgentFailure { timestamp: ts });
            }
            lutin_llm::Message::Summary { .. } => {
                out.push(MessageMeta::Summary { timestamp: ts });
            }
            lutin_llm::Message::Assistant { text, thinking, tool_calls } => {
                if thinking.as_deref().is_some_and(|s| !s.is_empty()) {
                    let s = entry.metrics.thinking.unwrap_or_default();
                    out.push(MessageMeta::Thinking {
                        timestamp: ts.clone(),
                        ttft_ms: s.ttft_ms,
                        duration_ms: s.duration_ms,
                    });
                }
                if !text.is_empty() {
                    let s = entry.metrics.text.unwrap_or_default();
                    out.push(MessageMeta::Assistant {
                        timestamp: ts.clone(),
                        ttft_ms: s.ttft_ms,
                        duration_ms: s.duration_ms,
                        prompt_tokens: s.prompt_tokens,
                        completion_tokens: s.completion_tokens,
                    });
                }
                for (i, _call) in tool_calls.iter().enumerate() {
                    let stats = entry.metrics.tools.get(i).cloned().unwrap_or_default();
                    out.push(MessageMeta::Tool {
                        timestamp: stats.timestamp,
                        duration_ms: stats.duration_ms,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Translate the SDK's terminal reason to the chat protocol's. Shared
/// between the streaming `Finished` handler and the post-loop join
/// fallback so they can't drift.
pub(crate) fn map_finish_reason(reason: AgentFinishReason) -> FinishReason {
    match reason {
        AgentFinishReason::Stopped => FinishReason::Completed,
        AgentFinishReason::MaxRounds => FinishReason::MaxRounds,
        AgentFinishReason::Cancelled => FinishReason::Cancelled,
        AgentFinishReason::LoopDetected => FinishReason::Failed("loop detected".into()),
        AgentFinishReason::Error(e) => FinishReason::Failed(format!("{e}")),
        other => {
            warn!(?other, "unrecognized AgentFinishReason variant");
            FinishReason::Failed("unrecognized AgentFinishReason".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MessageMetrics, TextStats, ToolStats};
    use lutin_llm::{Message, ToolCall};

    fn assistant_with_calls(text: &str, calls: Vec<ToolCall>) -> Message {
        Message::Assistant {
            text: text.into(),
            thinking: None,
            tool_calls: calls,
        }
    }

    #[test]
    fn project_metrics_aligns_with_project_history() {
        let call = ToolCall {
            id: lutin_llm::CallId::new("c1"),
            name: lutin_llm::ToolName::new("read"),
            arguments: serde_json::json!({}),
        };
        let entries = vec![
            Entry {
                message: Message::User("hi".into()),
                metrics: MessageMetrics {
                    timestamp: Some("t0".into()),
                    ..Default::default()
                },
            },
            Entry {
                message: assistant_with_calls("hello", vec![call]),
                metrics: MessageMetrics {
                    timestamp: Some("t1".into()),
                    text: Some(TextStats {
                        ttft_ms: Some(10),
                        duration_ms: Some(50),
                        prompt_tokens: Some(5),
                        completion_tokens: Some(3),
                    }),
                    tools: vec![ToolStats {
                        timestamp: Some("t1.5".into()),
                        duration_ms: Some(30),
                    }],
                    ..Default::default()
                },
            },
        ];
        let history = project_history(&entries);
        let metrics = project_metrics(&entries);
        assert_eq!(history.len(), metrics.len(), "1:1 alignment");
        assert!(matches!(metrics[0], MessageMeta::User { .. }));
        assert!(matches!(metrics[1], MessageMeta::Assistant { .. }));
        assert!(matches!(metrics[2], MessageMeta::Tool { .. }));
    }
}
