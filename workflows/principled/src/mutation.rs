//! In-place transcript mutation ops (Edit / Delete / DeleteFrom).
//!
//! Operates on `Vec<Entry>` directly, then mirrors the new message
//! list into the agent (when one exists) and persists. The projected
//! indices from the wire are resolved against the same projection
//! `project_history` uses (see [`ProjectedSlot`]) so the chrome's
//! "click on row N" maps unambiguously back to entry+slot.
//!
//! The `ReviewInFlight` guard is checked upstream in
//! `run_agent_loop`; this module assumes the caller already cleared
//! that gate.

use lutin_agent_sdk::Agent;
use principled::{ChatError, ChatEvent};
use tracing::warn;

use crate::projection::{build_summary_updated, project_history, project_metrics, write_summary};
use crate::runner::RunnerCtx;
use crate::store::{self, Entry};

/// Wire-level mutation op. Shared between `wire.rs` (which constructs
/// the value from a `ChatRequest`) and `runner.rs` (which dispatches
/// the variant on the `AgentCmd::Mutate` arm).
#[derive(Debug, Clone)]
pub(crate) enum MutateOp {
    Edit { index: u32, text: String },
    Delete { index: u32 },
    DeleteFrom { index: u32 },
}

/// Apply one mutation op to the canonical history. Mutates the in-memory
/// `entries` vec, then mirrors the new message list into the agent (if
/// one exists) and persists. Each `Entry` carries its own metrics, so
/// mutations move data and metrics together — there's no parallel-vec
/// realignment step.
pub(crate) fn apply_mutation(
    ctx: &RunnerCtx,
    agent: Option<&mut Agent>,
    entries: &mut Vec<Entry>,
    op: MutateOp,
) -> Result<(), ChatError> {
    if let Some(a) = agent {
        // Reject the mutation up-front when a turn is streaming. The
        // SDK's edit_messages will also reject, but its error is opaque
        // — checking here lets the UI surface `TurnInFlight` cleanly.
        let mut applied: Result<(), ChatError> = Ok(());
        a.edit_messages(|_| {
            applied = mutate_entries(entries, &op);
        })
        .map_err(|_| ChatError::TurnInFlight)?;
        applied?;
        // Now sync the mutated message list back into the agent.
        let msgs = store::messages(entries);
        a.edit_messages(|m| *m = msgs)
            .map_err(|_| ChatError::TurnInFlight)?;
    } else {
        mutate_entries(entries, &op)?;
    }
    store::save(&ctx.state_dir, entries).map_err(|e| {
        warn!(error = %e, "save transcript after mutation failed");
        ChatError::PersistFailed { op: "save transcript".into() }
    })?;
    write_summary(&ctx.state_dir, &ctx.resolver, entries);
    let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    Ok(())
}

/// Apply `op` to `entries`. Edit/Delete/DeleteFrom find the target via
/// the projected-index → entry-index mapping, then operate on the entry
/// in place. Tool-call slot edits are rejected as out-of-range so the
/// UI can disable the menu.
pub(crate) fn mutate_entries(entries: &mut Vec<Entry>, op: &MutateOp) -> Result<(), ChatError> {
    use lutin_llm::Message;
    let (entry_idx, slot) = locate_entry(entries, projected_index(op))?;
    match op {
        MutateOp::Edit { text, .. } => match (&mut entries[entry_idx].message, slot) {
            (Message::User(t), ProjectedSlot::User) => *t = text.clone(),
            (Message::Assistant { thinking, .. }, ProjectedSlot::Thinking) => {
                *thinking = Some(text.clone());
            }
            (Message::Assistant { text: at, .. }, ProjectedSlot::AssistantText) => {
                *at = text.clone();
            }
            (_, ProjectedSlot::Tool | ProjectedSlot::SubAgent) => {
                return Err(ChatError::HistoryIndexOutOfRange(projected_index(op)));
            }
            _ => unreachable!("slot resolved against same entries"),
        },
        MutateOp::Delete { .. } => match slot {
            ProjectedSlot::User => {
                entries.remove(entry_idx);
            }
            ProjectedSlot::Thinking => {
                if let Message::Assistant { thinking, .. } = &mut entries[entry_idx].message {
                    *thinking = None;
                }
                entries[entry_idx].metrics.thinking = None;
            }
            ProjectedSlot::AssistantText => {
                if let Message::Assistant { text, .. } = &mut entries[entry_idx].message {
                    text.clear();
                }
                entries[entry_idx].metrics.text = None;
            }
            ProjectedSlot::Tool | ProjectedSlot::SubAgent => {
                return Err(ChatError::HistoryIndexOutOfRange(projected_index(op)));
            }
        },
        MutateOp::DeleteFrom { .. } => {
            entries.truncate(entry_idx);
        }
    }
    Ok(())
}

fn projected_index(op: &MutateOp) -> u32 {
    match op {
        MutateOp::Edit { index, .. }
        | MutateOp::Delete { index }
        | MutateOp::DeleteFrom { index } => *index,
    }
}

/// Which field of an underlying `Message` a projected history entry
/// addresses. Mirrors the iteration order in `project_history`.
#[derive(Debug, Clone, Copy)]
enum ProjectedSlot {
    User,
    Thinking,
    AssistantText,
    /// One entry per `Assistant.tool_calls[idx]`. Rejected by
    /// `mutate_entries` — tool exchanges aren't user-editable.
    Tool,
    /// `Message::SubAgentReply` or `Message::SubAgentFailure`.
    SubAgent,
}

/// Walk `entries` in projected order, yielding `(entry_index, slot)`
/// for each visible row.
fn projected_slots(entries: &[Entry]) -> impl Iterator<Item = (usize, ProjectedSlot)> + '_ {
    use lutin_llm::Message;
    entries.iter().enumerate().flat_map(|(i, e)| {
        let user = matches!(&e.message, Message::User(t) if !t.is_empty())
            .then_some(ProjectedSlot::User);
        let sub_agent = matches!(
            &e.message,
            Message::SubAgentReply { .. } | Message::SubAgentFailure { .. }
        )
        .then_some(ProjectedSlot::SubAgent);
        let (thinking, text, tools_count) = match &e.message {
            Message::Assistant { text, thinking, tool_calls } => (
                thinking
                    .as_deref()
                    .is_some_and(|s| !s.is_empty())
                    .then_some(ProjectedSlot::Thinking),
                (!text.is_empty()).then_some(ProjectedSlot::AssistantText),
                tool_calls.len(),
            ),
            _ => (None, None, 0),
        };
        user.into_iter()
            .chain(sub_agent)
            .chain(thinking)
            .chain(text)
            .chain(std::iter::repeat(ProjectedSlot::Tool).take(tools_count))
            .map(move |s| (i, s))
    })
}

fn locate_entry(
    entries: &[Entry],
    index: u32,
) -> Result<(usize, ProjectedSlot), ChatError> {
    projected_slots(entries)
        .nth(index as usize)
        .ok_or(ChatError::HistoryIndexOutOfRange(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MessageMetrics;
    use lutin_llm::{Message, ToolCall};

    fn assistant_with_calls(text: &str, calls: Vec<ToolCall>) -> Message {
        Message::Assistant {
            text: text.into(),
            thinking: None,
            tool_calls: calls,
        }
    }

    fn entry(message: Message) -> Entry {
        Entry { message, metrics: MessageMetrics::default() }
    }

    #[test]
    fn mutate_entries_delete_user_drops_entry_and_metrics_together() {
        let mut entries = vec![
            entry(Message::User("first".into())),
            entry(Message::User("second".into())),
        ];
        entries[0].metrics.timestamp = Some("t0".into());
        entries[1].metrics.timestamp = Some("t1".into());
        mutate_entries(&mut entries, &MutateOp::Delete { index: 0 }).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].metrics.timestamp.as_deref(), Some("t1"));
    }

    #[test]
    fn mutate_entries_delete_from_truncates_both_halves() {
        let mut entries = vec![
            entry(Message::User("a".into())),
            entry(Message::User("b".into())),
            entry(Message::User("c".into())),
        ];
        entries[2].metrics.timestamp = Some("t2".into());
        mutate_entries(&mut entries, &MutateOp::DeleteFrom { index: 1 }).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn mutate_entries_edit_user_text() {
        let mut entries = vec![entry(Message::User("old".into()))];
        mutate_entries(
            &mut entries,
            &MutateOp::Edit { index: 0, text: "new".into() },
        )
        .unwrap();
        match &entries[0].message {
            Message::User(t) => assert_eq!(t, "new"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mutate_entries_rejects_tool_edit() {
        let call = ToolCall {
            id: lutin_llm::CallId::new("c1"),
            name: lutin_llm::ToolName::new("read"),
            arguments: serde_json::json!({}),
        };
        let mut entries = vec![entry(assistant_with_calls("calling", vec![call]))];
        let result = mutate_entries(
            &mut entries,
            &MutateOp::Edit { index: 1, text: "no".into() },
        );
        assert!(matches!(result, Err(ChatError::HistoryIndexOutOfRange(1))));
    }
}
