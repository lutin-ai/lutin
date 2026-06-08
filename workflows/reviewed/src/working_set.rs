//! Keeps the agent's view of files fresh and bounded.
//!
//! Passes, all run after a tool call executes:
//!
//!   - **refresh** — when a write/edit lands on a file the agent previously
//!     `read`, the stale read pair(s) are dropped and the read is re-run
//!     (same args as the most recent one) with the fresh result appended at
//!     the recent end of history. The model never edits against an outdated
//!     copy of the file.
//!   - **compact** — duplicate reads collapse to the newest; superseded
//!     writes (an older write to a file with a fresher read after it) are
//!     dropped; only the last `KEEP_READS` read pairs are kept.
//!
//! All passes only touch the empty-text assistant+call pairs the slot
//! rewind produces, so prose-bearing assistant messages are never removed.

use std::collections::{HashMap, HashSet};

use lutin_llm::{CallId, Message, ToolCall, ToolName};
use lutin_tools::{ToolCallContext, ToolResult};
use tracing::info;

use crate::types::Agent;

const KEEP_READS: usize = 6;
const READ: &str = "read";

pub fn is_edit_tool(name: &str) -> bool {
    matches!(name, "write" | "edit" | "edit_lines")
}

/// Re-run the most recent `read` of the file this edit touched and swap it
/// in for every stale read pair of that file. No-op if the file was never
/// read, or if the fresh read fails (stale reads are then left in place —
/// an outdated view beats no view).
pub async fn refresh_after_edit(agent: &mut Agent, edit: &ToolCall) {
    if !is_edit_tool(edit.name.as_str()) {
        return;
    }
    let Some(path) = edit.arguments.get("path").and_then(|v| v.as_str()) else {
        return;
    };
    let path = norm(path).to_string();

    let stale: Vec<usize> = read_pairs(&agent.messages)
        .into_iter()
        .filter(|&i| pair_path(&agent.messages, i).is_some_and(|p| norm(p) == path))
        .collect();
    let Some(&last) = stale.last() else { return };
    let Message::Assistant { tool_calls, .. } = &agent.messages[last] else {
        return;
    };
    let fresh_call = ToolCall {
        id: CallId::new(format!("ws-{}", edit.id)),
        name: ToolName::new(READ),
        arguments: tool_calls[0].arguments.clone(),
    };

    let ctx = ToolCallContext::default();
    let result = agent.toolbox.call(&ctx, fresh_call.clone()).await;
    let ToolResult::Ok(rc) = result else { return };
    if rc.is_error {
        return;
    }

    for &i in stale.iter().rev() {
        agent.messages.drain(i..=i + 1);
    }
    agent.messages.push(Message::Assistant {
        text: String::new(),
        tool_calls: vec![fresh_call],
        thinking: None,
        thinking_signature: None,
    });
    agent.messages.push(Message::ToolResult(rc));
    info!(stage = "working_set", path, dropped = stale.len(), "refreshed read after edit");
}

pub fn compact(messages: &mut Vec<Message>) {
    dedup_reads(messages);
    prune_old_writes(messages);
    evict_old_reads(messages);
}

/// Keep only the newest `KEEP_READS` read pairs; drop older ones entirely.
fn evict_old_reads(messages: &mut Vec<Message>) {
    let pairs = read_pairs(messages);
    if pairs.len() <= KEEP_READS {
        return;
    }
    for &i in pairs[..pairs.len() - KEEP_READS].iter().rev() {
        messages.drain(i..=i + 1);
    }
}

/// Drop duplicate reads (same arguments), keeping the newest.
fn dedup_reads(messages: &mut Vec<Message>) {
    let pairs = read_pairs(messages);
    let mut seen = HashSet::new();
    let mut drop = Vec::new();
    for &i in pairs.iter().rev() {
        let Message::Assistant { tool_calls, .. } = &messages[i] else {
            continue;
        };
        if !seen.insert(tool_calls[0].arguments.to_string()) {
            drop.push(i);
        }
    }
    for &i in drop.iter() {
        messages.drain(i..=i + 1);
    }
}

/// Drop write/edit pairs superseded by both a later write to the same file
/// and a later read of it. The newest write per file always survives (it
/// carries the format/check diagnostics and anchors "I just changed this"),
/// and nothing is pruned unless a fresher read shows the resulting state.
fn prune_old_writes(messages: &mut Vec<Message>) {
    let reads = read_pairs(messages);
    let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
    for i in tool_pairs(messages, is_edit_tool) {
        if let Some(p) = pair_path(messages, i) {
            by_path.entry(norm(p).to_string()).or_default().push(i);
        }
    }
    let mut drop = Vec::new();
    for (path, idxs) in &by_path {
        for &i in &idxs[..idxs.len() - 1] {
            let read_after = reads
                .iter()
                .any(|&r| r > i && pair_path(messages, r).is_some_and(|p| norm(p) == *path));
            if read_after {
                drop.push(i);
            }
        }
    }
    drop.sort_unstable();
    for &i in drop.iter().rev() {
        messages.drain(i..=i + 1);
    }
}

fn read_pairs(messages: &[Message]) -> Vec<usize> {
    tool_pairs(messages, |n| n == READ)
}

/// Indices of every adjacent (assistant-with-lone-matching-call, tool-result)
/// pair. Only empty-text assistants qualify — the shape the slot rewind
/// writes — so messages carrying prose are never candidates for removal.
fn tool_pairs(messages: &[Message], pred: impl Fn(&str) -> bool) -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..messages.len().saturating_sub(1) {
        let Message::Assistant { text, tool_calls, .. } = &messages[i] else {
            continue;
        };
        if !text.is_empty() || tool_calls.len() != 1 || !pred(tool_calls[0].name.as_str()) {
            continue;
        }
        let Message::ToolResult(rc) = &messages[i + 1] else {
            continue;
        };
        if rc.call_id == tool_calls[0].id {
            out.push(i);
        }
    }
    out
}

fn pair_path(messages: &[Message], i: usize) -> Option<&str> {
    let Message::Assistant { tool_calls, .. } = &messages[i] else {
        return None;
    };
    tool_calls[0].arguments.get("path").and_then(|v| v.as_str())
}

fn norm(path: &str) -> &str {
    let p = path.trim();
    p.strip_prefix("./").unwrap_or(p)
}
