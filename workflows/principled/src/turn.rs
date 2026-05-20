//! One turn from user message (or rerun trigger) to terminal
//! `MessageFinished`.
//!
//! Owns the rewind/round inner loops, the per-turn `TurnTracker`,
//! and the agent-event projection. Calls into `rewind`,
//! `compaction`, `projection`, and `mutation` for the heavy lifting.

use std::time::Instant;

use futures_util::StreamExt;
use lutin_agent_sdk::{Agent, AgentEvent, ToolResult};
use lutin_workflow_sdk::agent::refresh_agent as sdk_refresh_agent;
use lutin_workflow_sdk::summary as sdk_summary;
use principled::{ChatError, ChatEvent, FinishReason, ToolOutcome, TurnId};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

use crate::agent_build::{map_build_error, resolve_args};
use crate::compaction::run_compaction;
use crate::projection::{
    build_summary_updated, entry_tokens, map_finish_reason, project_history, project_metrics,
    write_summary,
};
use crate::review;
use crate::rewind::{PendingRewind, perform_rewind, squash_denied_attempts, sync_new_entries};
use crate::runner::{AgentCmd, RunnerCtx};
use crate::store::{
    self, Entry, MessageMetrics, TextStats, ThinkingStats, ToolStats, now_rfc3339,
};
use crate::subagents_glue::{broadcast_subagents, build_prompt_extras, subagent_block};

/// In-memory bookkeeping for a single turn's metrics. Created at the
/// top of `run_turn`, harvested into the corresponding `Entry` rows
/// after the turn ends.
pub(crate) struct TurnTracker {
    pub(crate) started_at: Instant,
    pub(crate) first_text_at: Option<Instant>,
    pub(crate) first_thinking_at: Option<Instant>,
    pub(crate) last_usage: Option<lutin_llm::Usage>,
    pub(crate) total_prompt_pre_turn: u64,
    pub(crate) total_completion_pre_turn: u64,
    pub(crate) intra_turn_prompt: u64,
    pub(crate) intra_turn_completion: u64,
    pub(crate) model_active_ms: u64,
    pub(crate) current_round_started: Option<Instant>,
    pub(crate) tools: Vec<ToolLifecycle>,
}

pub(crate) struct ToolLifecycle {
    pub(crate) call_id: String,
    pub(crate) started_at: Instant,
    pub(crate) started_ts: String,
    pub(crate) finished_at: Option<Instant>,
}

impl TurnTracker {
    pub(crate) fn new(pre_turn: sdk_summary::SummaryTotals) -> Self {
        Self {
            started_at: Instant::now(),
            first_text_at: None,
            first_thinking_at: None,
            last_usage: None,
            total_prompt_pre_turn: pre_turn.total_prompt_tokens,
            total_completion_pre_turn: pre_turn.total_completion_tokens,
            intra_turn_prompt: 0,
            intra_turn_completion: 0,
            model_active_ms: 0,
            current_round_started: None,
            tools: Vec::new(),
        }
    }
}

/// `text` is `Some(_)` for a new user message and `None` for a Rerun,
/// which kicks the agent loop against the existing transcript.
pub(crate) async fn run_turn(
    ctx: &RunnerCtx,
    rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
    agent: &mut Agent,
    entries: &mut Vec<Entry>,
    review_session: &mut Option<review::ReviewSession>,
    text: Option<String>,
    turn: TurnId,
) {
    let resolved = match resolve_args(ctx, None) {
        Ok(r) => r,
        Err(e) => {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("{e}")),
            });
            return;
        }
    };
    let extras = build_prompt_extras(ctx, entries, &resolved.persona).await;
    let (rewind_tx, mut rewind_rx) = mpsc::unbounded_channel::<review::RewindSignal>();
    let (review_req_tx, mut review_req_rx) =
        mpsc::unbounded_channel::<review::ReviewRequest>();
    if let Err(e) = sdk_refresh_agent(
        agent,
        resolved.as_build_args_with(ctx, extras, None, Some(rewind_tx.clone())),
    ) {
        let _ = ctx.events.send(ChatEvent::MessageFinished {
            turn_id: turn,
            reason: FinishReason::Failed(format!("{}", map_build_error(e))),
        });
        return;
    }
    if let Some(block) = subagent_block(ctx).await {
        let _ = agent.update_config(|cfg| {
            if cfg.system.is_empty() {
                cfg.system = block;
            } else {
                cfg.system.push_str("\n\n");
                cfg.system.push_str(&block);
            }
        });
    }
    let principle_names: Vec<String> = match crate::principle::Principle::list(&ctx.resolver) {
        Ok(installed) => {
            let installed: std::collections::HashSet<&str> =
                installed.iter().map(|p| p.name.as_str()).collect();
            let mut out = Vec::with_capacity(crate::principle::WORKFLOW_ORDER.len());
            for name in crate::principle::WORKFLOW_ORDER.iter() {
                if installed.contains(name) {
                    out.push((*name).to_string());
                } else {
                    tracing::warn!(
                        principle = name,
                        "principles.toml lists a principle that isn't installed; skipping"
                    );
                }
            }
            out
        }
        Err(e) => {
            tracing::warn!(error = %e, "Principle::list failed; turn runs ungated");
            Vec::new()
        }
    };
    if !principle_names.is_empty() {
        let install = (|| -> Result<(), String> {
            let (policy, session) = review::build(
                &ctx.resolver,
                &resolved.settings,
                &principle_names,
                review_req_tx.clone(),
                rewind_tx,
                ctx.events.clone(),
                ctx.state_dir.clone(),
                resolved.review_concurrency,
            )
            .map_err(|e| format!("build review policy: {e}"))?;
            match review_session.as_mut() {
                Some(existing) => {
                    existing.bundles = session.bundles;
                    existing.rewind_tx = session.rewind_tx;
                }
                None => *review_session = Some(session),
            }
            agent
                .try_set_approval(Box::new(policy))
                .map_err(|e| format!("install review policy: {e}"))?;
            agent
                .update_config(|cfg| cfg.tool_policy.max_calls_per_round = 1)
                .map_err(|e| format!("set max_calls_per_round: {e}"))?;
            Ok(())
        })();
        if let Err(reason) = install {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(reason),
            });
            return;
        }
    } else {
        *review_session = None;
    }
    drop(review_req_tx);
    run_compaction(ctx, agent, &resolved, entries).await;
    if let Some(text) = text {
        let user_msg = lutin_llm::Message::User(text);
        if let Err(e) = agent.push_message(user_msg.clone()) {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("push: {e}")),
            });
            return;
        }
        entries.push(Entry {
            message: user_msg,
            metrics: MessageMetrics {
                timestamp: Some(now_rfc3339()),
                ..Default::default()
            },
        });
        if let Err(e) = store::save(&ctx.state_dir, entries) {
            warn!(error = %e, "save transcript after user push failed");
        }
        let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
        let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
        let _ = ctx.events.send(build_summary_updated(entries));
    }
    let pre_turn_len = entries.len();
    let mut tracker = TurnTracker::new(sdk_summary::aggregate(entry_tokens(entries)));
    let mut live_messages_len: usize = pre_turn_len;
    let mut stream = match agent.start() {
        Ok(s) => s,
        Err(e) => {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("start: {e}")),
            });
            return;
        }
    };

    let mut finish: Option<FinishReason> = None;
    let mut pending_feedback: Option<PendingRewind> = None;
    let mut user_cancelled = false;
    let mut last_round_self_abort = false;
    let outcome = 'rewind: loop {
        'round: loop {
            tokio::select! {
                ev = stream.next() => match ev {
                    Some(ev) => {
                        update_live_messages_len(&mut live_messages_len, &ev);
                        if let AgentEvent::ToolCallCompleted { call, .. } = &ev
                            && call.name.as_str() != review::ABORT_STEP_TOOL_NAME
                        {
                            last_round_self_abort = false;
                        }
                        if let Some(reason) = handle_agent_event(ev, &ctx.events, &mut tracker) {
                            finish = Some(reason);
                        }
                    }
                    None => break 'round,
                },
                cmd = rx.recv() => match cmd {
                    Some(AgentCmd::Cancel) => {
                        user_cancelled = true;
                        pending_feedback = None;
                        agent.cancel();
                    }
                    Some(AgentCmd::Send { turn: dropped_turn, .. })
                    | Some(AgentCmd::Rerun { turn: dropped_turn }) => {
                        warn!("send/rerun received during in-flight turn — dropping; client should wait");
                        let _ = ctx.events.send(ChatEvent::MessageFinished {
                            turn_id: dropped_turn,
                            reason: FinishReason::Failed("turn already in flight".into()),
                        });
                    }
                    Some(AgentCmd::Mutate { reply, .. }) => {
                        let _ = reply.send(Err(ChatError::TurnInFlight));
                    }
                    None => {
                        agent.cancel();
                    }
                },
                Some(req) = review_req_rx.recv() => {
                    if let Some(s) = review_session.as_mut() {
                        s.handle(req, live_messages_len);
                    } else {
                        debug_assert!(false, "review request without an active session");
                    }
                }
                Some(signal) = rewind_rx.recv(), if !user_cancelled => {
                    match signal {
                        review::RewindSignal::Continue { feedback } => {
                            if !matches!(pending_feedback, Some(PendingRewind::Abort { .. })) {
                                pending_feedback = Some(PendingRewind::Continue { feedback });
                            }
                        }
                        review::RewindSignal::Abort { reason } => {
                            pending_feedback = Some(PendingRewind::Abort { reason });
                        }
                    }
                    agent.cancel();
                }
            }
        }
        let outcome = agent.join().await;
        if user_cancelled {
            finish = Some(FinishReason::Cancelled);
            break 'rewind outcome;
        }
        let Some(pending) = pending_feedback.take() else {
            break 'rewind outcome;
        };
        if let PendingRewind::Continue { feedback } = &pending
            && feedback.starts_with(review::SELF_ABORT_FEEDBACK_PREFIX)
        {
            if last_round_self_abort {
                finish = Some(FinishReason::Failed(format!(
                    "agent self-aborted twice in a row — stopping so you can add context: {feedback}"
                )));
                break 'rewind outcome;
            }
            last_round_self_abort = true;
        } else {
            last_round_self_abort = false;
        }
        let (label, restart_after_rewind) = match &pending {
            PendingRewind::Continue { feedback } => (feedback.as_str(), true),
            PendingRewind::Abort { reason } => (reason.as_str(), false),
        };
        match perform_rewind(
            agent,
            entries,
            review_session.as_mut(),
            &mut live_messages_len,
            &ctx.events,
            label,
        ) {
            Ok(rewound) if restart_after_rewind => {
                if !rewound {
                    let PendingRewind::Continue { feedback } = &pending else {
                        unreachable!("restart_after_rewind implies Continue")
                    };
                    let synthetic = format!(
                        "[self-abort] You called abort_step but there is no prior step to \
                         rewind to. Reconsider from your current position. {feedback}"
                    );
                    let user_msg = lutin_llm::Message::User(synthetic);
                    if let Err(e) = agent.push_message(user_msg.clone()) {
                        finish = Some(FinishReason::Failed(format!(
                            "push self-abort prompt: {e}"
                        )));
                        break 'rewind outcome;
                    }
                    entries.push(Entry {
                        message: user_msg,
                        metrics: MessageMetrics {
                            timestamp: Some(now_rfc3339()),
                            ..Default::default()
                        },
                    });
                    live_messages_len = entries.len();
                    let _ = ctx
                        .events
                        .send(ChatEvent::HistoryReplaced(project_history(entries)));
                    let _ = ctx
                        .events
                        .send(ChatEvent::MetricsReplaced(project_metrics(entries)));
                    let _ = ctx.events.send(build_summary_updated(entries));
                }
                tracker = TurnTracker::new(sdk_summary::aggregate(entry_tokens(entries)));
                stream = match agent.start() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ctx.events.send(ChatEvent::MessageFinished {
                            turn_id: turn,
                            reason: FinishReason::Failed(format!("restart after rewind: {e}")),
                        });
                        return;
                    }
                };
            }
            Ok(_) => {
                let PendingRewind::Abort { reason } = pending else {
                    unreachable!("non-restart path implies Abort")
                };
                finish = Some(FinishReason::Failed(format!(
                    "review system failure: {reason}"
                )));
                break 'rewind outcome;
            }
            Err(e) => {
                warn!(error = %e, "perform_rewind failed; ending turn");
                finish = Some(FinishReason::Failed(format!("rewind failed: {e}")));
                break 'rewind outcome;
            }
        }
    };
    if let Err(e) = agent.edit_messages(|m| squash_denied_attempts(m, pre_turn_len)) {
        warn!(error = %e, "squash denied attempts failed (agent busy)");
    }
    sync_new_entries(agent.messages(), entries);
    finalize_turn_meta(entries, pre_turn_len, &tracker);
    if let Err(e) = store::save(&ctx.state_dir, entries) {
        warn!(error = %e, "save transcript failed");
    }
    write_summary(&ctx.state_dir, &ctx.resolver, entries);
    let reason = finish.unwrap_or_else(|| map_finish_reason(outcome.finish_reason));
    let _ = ctx.events.send(ChatEvent::MessageFinished {
        turn_id: turn,
        reason,
    });
    let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    broadcast_subagents(ctx).await;
}

/// Track the agent's live transcript length from the event stream.
fn update_live_messages_len(len: &mut usize, ev: &AgentEvent) {
    let delta = match ev {
        AgentEvent::AssistantMessage(_) => 1,
        AgentEvent::ToolCallCompleted { .. } => 1,
        _ => return,
    };
    *len = len.saturating_add(delta);
}

/// Translate one [`AgentEvent`] to zero-or-more [`ChatEvent`]s; returns
/// the terminal `FinishReason` when the agent's run ends.
fn handle_agent_event(
    ev: AgentEvent,
    events: &broadcast::Sender<ChatEvent>,
    tracker: &mut TurnTracker,
) -> Option<FinishReason> {
    match ev {
        AgentEvent::AssistantText(s) => {
            if tracker.first_text_at.is_none() {
                tracker.first_text_at = Some(Instant::now());
            }
            let _ = events.send(ChatEvent::Delta(s));
            None
        }
        AgentEvent::AssistantReasoning(s) => {
            if tracker.first_thinking_at.is_none() {
                tracker.first_thinking_at = Some(Instant::now());
            }
            let _ = events.send(ChatEvent::Reasoning(s));
            None
        }
        AgentEvent::ToolCallStreaming { id, name } => {
            let _ = events.send(ChatEvent::ToolCallStreaming {
                id: id.as_str().to_string(),
                name: name.as_str().to_string(),
            });
            None
        }
        AgentEvent::ToolCallArgsDelta { id, args } => {
            let _ = events.send(ChatEvent::ToolCallArgsDelta {
                id: id.as_str().to_string(),
                args,
            });
            None
        }
        AgentEvent::ToolCallArgsParsed(call) => {
            tracker.tools.push(ToolLifecycle {
                call_id: call.id.as_str().to_string(),
                started_at: Instant::now(),
                started_ts: now_rfc3339(),
                finished_at: None,
            });
            let arguments_json = serde_json::to_string(&call.arguments)
                .expect("serializing serde_json::Value is infallible");
            let _ = events.send(ChatEvent::ToolCallArgsParsed {
                id: call.id.as_str().to_string(),
                name: call.name.as_str().to_string(),
                arguments_json,
            });
            None
        }
        AgentEvent::ToolCallCompleted { call, outcome } => {
            if let Some(life) = tracker
                .tools
                .iter_mut()
                .rev()
                .find(|l| l.call_id == call.id.as_str())
            {
                life.finished_at = Some(Instant::now());
            }
            let chat_outcome = match outcome {
                ToolResult::Ok(c) if c.is_error => ToolOutcome::Failed(c.content),
                ToolResult::Ok(c) => ToolOutcome::Ok(c.content),
                ToolResult::Err(e) => ToolOutcome::Failed(format!("{e}")),
                other => {
                    warn!(?other, "unrecognized ToolResult variant");
                    ToolOutcome::Failed("unrecognized ToolResult variant".to_string())
                }
            };
            let _ = events.send(ChatEvent::ToolCallCompleted {
                id: call.id.as_str().to_string(),
                outcome: chat_outcome,
            });
            None
        }
        AgentEvent::Usage(u) => {
            tracker.intra_turn_prompt = tracker
                .intra_turn_prompt
                .saturating_add(u.prompt_tokens as u64);
            tracker.intra_turn_completion = tracker
                .intra_turn_completion
                .saturating_add(u.completion_tokens as u64);
            let _ = events.send(ChatEvent::SummaryUpdated {
                context_tokens: Some(u.prompt_tokens),
                total_prompt_tokens: tracker
                    .total_prompt_pre_turn
                    .saturating_add(tracker.intra_turn_prompt),
                total_completion_tokens: tracker
                    .total_completion_pre_turn
                    .saturating_add(tracker.intra_turn_completion),
            });
            tracker.last_usage = Some(u);
            None
        }
        AgentEvent::Finished(reason) => Some(map_finish_reason(reason)),
        AgentEvent::Error(e) => Some(FinishReason::Failed(format!("{e}"))),
        AgentEvent::RoundStarted { .. } => {
            tracker.current_round_started = Some(Instant::now());
            None
        }
        AgentEvent::AssistantMessage(_) => {
            if let Some(start) = tracker.current_round_started.take() {
                let elapsed = Instant::now()
                    .saturating_duration_since(start)
                    .as_millis() as u64;
                tracker.model_active_ms = tracker.model_active_ms.saturating_add(elapsed);
            }
            None
        }
        AgentEvent::RoundEnded { .. } => None,
        other => {
            warn!(?other, "unrecognized AgentEvent variant");
            None
        }
    }
}

/// Attach turn stats to entries added during this turn. The last
/// `Message::Assistant` in `pre_turn_len..` gets the full set (TTFT,
/// duration, tokens); intermediates keep just their timestamp. Tool
/// stats are populated by walking the tracker's lifecycles and
/// resolving each `call_id` back to its slot in an assistant entry's
/// `tool_calls`.
fn finalize_turn_meta(entries: &mut Vec<Entry>, pre_turn_len: usize, tracker: &TurnTracker) {
    let now = Instant::now();
    let total_ms = now.saturating_duration_since(tracker.started_at).as_millis() as u64;
    let duration_ms = if tracker.model_active_ms > 0 {
        tracker.model_active_ms
    } else {
        total_ms
    };
    let ttft_ms = tracker
        .first_text_at
        .map(|t1| t1.saturating_duration_since(tracker.started_at).as_millis() as u64);
    let thinking_ttft_ms = tracker
        .first_thinking_at
        .map(|t1| t1.saturating_duration_since(tracker.started_at).as_millis() as u64);
    let prompt_tokens = tracker.last_usage.as_ref().map(|u| u.prompt_tokens);
    let completion_tokens = if tracker.intra_turn_completion > 0 {
        Some(u32::try_from(tracker.intra_turn_completion).unwrap_or(u32::MAX))
    } else {
        tracker.last_usage.as_ref().map(|u| u.completion_tokens)
    };

    for life in &tracker.tools {
        let dur = life
            .finished_at
            .map(|t| t.saturating_duration_since(life.started_at).as_millis() as u64);
        let stats = ToolStats {
            timestamp: Some(life.started_ts.clone()),
            duration_ms: dur,
        };
        if let Some((entry_idx, slot)) = locate_tool_slot(entries, &life.call_id, pre_turn_len) {
            if let Some(out) = entries[entry_idx].metrics.tools.get_mut(slot) {
                *out = stats;
            }
        }
    }

    let last_assistant_idx = (pre_turn_len..entries.len())
        .rev()
        .find(|&i| matches!(entries[i].message, lutin_llm::Message::Assistant { .. }));
    let Some(idx) = last_assistant_idx else { return };
    let lutin_llm::Message::Assistant { text, thinking, .. } = &entries[idx].message else {
        return;
    };
    let has_text = !text.is_empty();
    let has_thinking = thinking.as_deref().is_some_and(|s| !s.is_empty());
    let metrics = &mut entries[idx].metrics;
    if has_text {
        metrics.text = Some(TextStats {
            ttft_ms,
            duration_ms: Some(duration_ms),
            prompt_tokens,
            completion_tokens,
        });
    }
    if has_thinking {
        metrics.thinking = Some(ThinkingStats {
            ttft_ms: thinking_ttft_ms,
            duration_ms: Some(duration_ms),
        });
    }
}

/// Find the assistant entry that owns a tool call with `call_id`,
/// returning its `(entry_index, slot_within_tool_calls)`.
fn locate_tool_slot(entries: &[Entry], call_id: &str, start: usize) -> Option<(usize, usize)> {
    for (i, e) in entries.iter().enumerate().skip(start) {
        if let lutin_llm::Message::Assistant { tool_calls, .. } = &e.message
            && let Some(pos) = tool_calls.iter().position(|c| c.id.as_str() == call_id)
        {
            return Some((i, pos));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use lutin_llm::{Message, ToolCall};

    fn assistant(text: &str) -> Message {
        Message::Assistant {
            text: text.into(),
            thinking: None,
            tool_calls: Vec::new(),
        }
    }

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
    fn finalize_turn_meta_attributes_to_last_assistant() {
        let mut entries = vec![
            entry(Message::User("hi".into())),
            entry(assistant("first")),
            entry(assistant("second")),
        ];
        let mut tracker = TurnTracker::new(sdk_summary::SummaryTotals::default());
        tracker.started_at = std::time::Instant::now() - std::time::Duration::from_millis(123);
        tracker.first_text_at = Some(std::time::Instant::now() - std::time::Duration::from_millis(80));
        tracker.last_usage = Some(lutin_llm::Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        });
        finalize_turn_meta(&mut entries, 1, &tracker);
        assert!(entries[1].metrics.text.is_none(), "intermediate has no stats");
        let last = entries[2].metrics.text.expect("last assistant has stats");
        assert_eq!(last.prompt_tokens, Some(100));
        assert_eq!(last.completion_tokens, Some(50));
        assert!(last.duration_ms.unwrap() >= 100);
        assert!(last.ttft_ms.unwrap() >= 30);
        assert!(last.ttft_ms.unwrap() <= 80);
    }

    #[test]
    fn finalize_turn_meta_records_tool_durations_by_call_id() {
        let call = ToolCall {
            id: lutin_llm::CallId::new("c1"),
            name: lutin_llm::ToolName::new("read"),
            arguments: serde_json::json!({}),
        };
        let mut entries = vec![
            entry(Message::User("hi".into())),
            entry(assistant_with_calls("doing", vec![call])),
        ];
        entries[1].metrics.tools = vec![ToolStats::default()];
        let mut tracker = TurnTracker::new(sdk_summary::SummaryTotals::default());
        let now = std::time::Instant::now();
        tracker.tools.push(ToolLifecycle {
            call_id: "c1".into(),
            started_at: now - std::time::Duration::from_millis(200),
            started_ts: "2026-05-08T12:00:00Z".into(),
            finished_at: Some(now - std::time::Duration::from_millis(50)),
        });
        finalize_turn_meta(&mut entries, 1, &tracker);
        let stats = &entries[1].metrics.tools[0];
        assert_eq!(stats.timestamp.as_deref(), Some("2026-05-08T12:00:00Z"));
        assert!(stats.duration_ms.unwrap() >= 100);
    }
}
