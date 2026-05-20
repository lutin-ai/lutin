//! Per-turn rewind and the post-turn transcript hygiene.
//!
//! [`perform_rewind`] is invoked when a reviewer signals a rethink: it
//! pops the failed step's frame, restores both file snapshots,
//! truncates the agent's messages and our `entries` to the prior
//! frame's `conversation_index`, and pushes a synthetic user-message
//! carrying the reviewer's feedback. [`squash_denied_attempts`] then
//! runs at the tail of every turn to drop rejected attempts from the
//! agent's transcript so the model's next context shows only the
//! accepted path.

use lutin_agent_sdk::Agent;
use principled::ChatEvent;
use tokio::sync::broadcast;

use crate::projection::{build_summary_updated, project_history, project_metrics};
use crate::review;
use crate::store::{Entry, MessageMetrics, ToolStats, now_rfc3339};

/// Outcome of the rewind channel for one turn iteration. `Continue`
/// restores the snapshot and restarts the agent (the existing rethink
/// path); `Abort` restores the snapshot and halts the turn with a
/// `Failed` reason — used when the review system itself can't make a
/// decision and auto-retrying would just amplify load against a
/// wedged backend.
#[derive(Debug, Clone)]
pub(crate) enum PendingRewind {
    Continue { feedback: String },
    Abort { reason: String },
}

/// Pop the top frame, restore both file snapshots, truncate the
/// agent's messages and our `entries` to the prior frame's
/// conversation_index, and append a synthetic user-message that hands
/// the carried_forward feedback to the agent. Returns `Ok(true)` when
/// the rewind succeeded and a new `agent.start()` should run; returns
/// `Ok(false)` when the failed step was the bottom of the stack
/// (caller should surface to user). Any IO / state error becomes
/// `Err`.
pub(crate) fn perform_rewind(
    agent: &mut Agent,
    entries: &mut Vec<Entry>,
    session: Option<&mut review::ReviewSession>,
    live_messages_len: &mut usize,
    events: &broadcast::Sender<ChatEvent>,
    feedback: &str,
) -> Result<bool, String> {
    let session = session.ok_or_else(|| "rewind requested with no active session".to_string())?;
    let outcome = session
        .stack
        .rewind(feedback)
        .map_err(|e| format!("rewind file restore: {e}"))?;
    let truncate_to = match outcome {
        crate::step::RewindOutcome::Rewound { reactivated } => session
            .stack
            .frames()
            .iter()
            .find(|f| f.id == reactivated)
            .map(|f| f.snapshot.conversation_index)
            .ok_or_else(|| "reactivated frame not found".to_string())?,
        crate::step::RewindOutcome::BottomOfStack => return Ok(false),
    };

    if let Err(e) = agent.edit_messages(|m| {
        if truncate_to <= m.len() {
            m.truncate(truncate_to);
        }
    }) {
        return Err(format!("agent.edit_messages: {e}"));
    }
    if truncate_to <= entries.len() {
        entries.truncate(truncate_to);
    }

    let synthetic = format!(
        "[review rewind] An earlier step was rolled back. Reconsider the most recent step \
         from a different angle. Reviewer feedback: {feedback}"
    );
    let user_msg = lutin_llm::Message::User(synthetic);
    if let Err(e) = agent.push_message(user_msg.clone()) {
        return Err(format!("agent.push_message: {e}"));
    }
    entries.push(Entry {
        message: user_msg,
        metrics: MessageMetrics {
            timestamp: Some(now_rfc3339()),
            ..Default::default()
        },
    });

    *live_messages_len = entries.len();

    let _ = events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = events.send(build_summary_updated(entries));
    Ok(true)
}

/// Drop rejected review attempts from the agent's transcript so the
/// final state shows only the accepted step's tool_use → tool_result.
/// Detection delegated to `review::is_review_denial` — that's the
/// shared contract with the policy that emits the denial. A genuine
/// in-tool error or a deny from some other future approval source
/// won't match.
///
/// The matching tool_call is pruned from the preceding `Assistant`;
/// if that leaves `tool_calls` empty the assistant message goes too,
/// taking any narration with it.
///
/// `start` is the pre-turn message count — we never touch history
/// from earlier turns.
pub(crate) fn squash_denied_attempts(messages: &mut Vec<lutin_llm::Message>, start: usize) {
    use lutin_llm::Message;
    let mut i = messages.len();
    while i > start {
        i -= 1;
        let denied_call_id = match &messages[i] {
            Message::ToolResult(tr) if tr.is_error && review::is_review_denial(&tr.content) => {
                Some(tr.call_id.clone())
            }
            _ => None,
        };
        let Some(call_id) = denied_call_id else { continue };
        messages.remove(i);
        if i == start {
            continue;
        }
        let j = i - 1;
        let drop_assistant = match &mut messages[j] {
            Message::Assistant { tool_calls, .. } => {
                tool_calls.retain(|c| c.id != call_id);
                tool_calls.is_empty()
            }
            _ => false,
        };
        if drop_assistant {
            messages.remove(j);
            i = j;
        }
    }
}

/// Append a fresh `Entry` for every message the agent added past
/// `entries.len()`. Each new entry gets a `now()` timestamp; per-stat
/// fields are filled in by `finalize_turn_meta`.
pub(crate) fn sync_new_entries(
    agent_messages: &[lutin_llm::Message],
    entries: &mut Vec<Entry>,
) {
    for msg in &agent_messages[entries.len()..] {
        let tools = match msg {
            lutin_llm::Message::Assistant { tool_calls, .. } => {
                vec![ToolStats::default(); tool_calls.len()]
            }
            _ => Vec::new(),
        };
        entries.push(Entry {
            message: msg.clone(),
            metrics: MessageMetrics {
                timestamp: Some(now_rfc3339()),
                tools,
                ..Default::default()
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lutin_llm::{Message, ToolCall};

    fn assistant_with_calls(text: &str, calls: Vec<ToolCall>) -> Message {
        Message::Assistant {
            text: text.into(),
            thinking: None,
            tool_calls: calls,
        }
    }

    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: lutin_llm::CallId::new(id),
            name: lutin_llm::ToolName::new(name),
            arguments: serde_json::json!({}),
        }
    }

    fn tool_result(call_id: &str, content: &str, is_error: bool) -> Message {
        Message::ToolResult(lutin_llm::ToolResultContent {
            call_id: lutin_llm::CallId::new(call_id),
            content: content.into(),
            is_error,
        })
    }

    #[test]
    fn squash_drops_denied_pair_and_keeps_accepted() {
        let mut msgs = vec![
            Message::User("hi".into()),
            assistant_with_calls("v1", vec![tool_call("c1", "edit")]),
            tool_result("c1", "denied: <review-deny> rejected by 'X': bad", true),
            assistant_with_calls("v2", vec![tool_call("c2", "edit")]),
            tool_result("c2", "applied", false),
        ];
        squash_denied_attempts(&mut msgs, 1);
        assert_eq!(msgs.len(), 3);
        match &msgs[1] {
            Message::Assistant { tool_calls, text, .. } => {
                assert_eq!(text, "v2");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id.as_str(), "c2");
            }
            _ => panic!("expected assistant"),
        }
        match &msgs[2] {
            Message::ToolResult(tr) => assert_eq!(tr.call_id.as_str(), "c2"),
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn squash_preserves_assistant_when_other_tool_calls_remain() {
        let mut msgs = vec![
            assistant_with_calls(
                "multi",
                vec![tool_call("c1", "edit"), tool_call("c2", "read")],
            ),
            tool_result("c1", "denied: <review-deny> rejected by 'X': bad", true),
            tool_result("c2", "ok", false),
        ];
        squash_denied_attempts(&mut msgs, 0);
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            Message::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id.as_str(), "c2");
            }
            _ => panic!("expected assistant"),
        }
    }

    #[test]
    fn squash_ignores_real_tool_errors() {
        let mut msgs = vec![
            assistant_with_calls("v1", vec![tool_call("c1", "edit")]),
            tool_result("c1", "file not found", true),
        ];
        let before_len = msgs.len();
        squash_denied_attempts(&mut msgs, 0);
        assert_eq!(msgs.len(), before_len);
        match &msgs[1] {
            Message::ToolResult(tr) => {
                assert!(tr.is_error);
                assert_eq!(tr.content, "file not found");
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn squash_does_not_touch_pre_turn_history() {
        let mut msgs = vec![
            assistant_with_calls("old", vec![tool_call("c0", "edit")]),
            tool_result("c0", "denied: rejected by 'old': bad", true),
            Message::User("new turn".into()),
            assistant_with_calls("v1", vec![tool_call("c1", "edit")]),
            tool_result("c1", "applied", false),
        ];
        squash_denied_attempts(&mut msgs, 2);
        assert_eq!(msgs.len(), 5, "pre-turn portion untouched");
    }
}
