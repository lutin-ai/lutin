//! Engine-side glue between the orchestrator session and the
//! sub-agent registry actor.
//!
//! Three responsibilities:
//! 1. Drive a sub-agent task to completion (`run_subagent_task`),
//!    translating SDK events to `AgentUpdate`s on the registry's
//!    update channel.
//! 2. Render the `<active_subagents>` system-prompt block and the
//!    `PromptExtras::attached_agents` list every time the main turn
//!    refreshes its agent.
//! 3. On terminal sub-agent transitions, inject a synthetic message
//!    into the parent's transcript and trigger an auto-turn so the
//!    orchestrator's LLM gets to react.

use lutin_agent_sdk::{Agent, AgentEvent, FinishReason as AgentFinishReason, ToolResult};
use lutin_entities::Persona;
use lutin_workflow_sdk::prompt::{
    AgentEntry as PromptAgentEntry, PersonaEntry as PromptPersonaEntry, PromptExtras,
};
use principled::{ChatEvent, FinishReason, SubAgentInfo, SubAgentStatus};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::agent_build;
use crate::agents;
use crate::projection::{
    build_summary_updated, map_finish_reason, project_history, project_messages, project_metrics,
    write_summary,
};
use crate::review;
use crate::runner::{AgentCmd, RunnerCtx, ensure_agent};
use crate::store::{self, Entry, MessageMetrics, now_rfc3339};
use crate::turn::run_turn;

/// Drive one sub-agent run to completion, translating SDK events into
/// `AgentUpdate`s on `update_tx`. Cancellation is via `AbortHandle` on
/// the outer task — the registry's `Stop` aborts us mid-poll, so we
/// don't observe `FinishReason::Cancelled` here (it's only reachable
/// when a future revision wires `agent.cancel()` into the cancel path).
pub(crate) async fn run_subagent_task(
    ctx: RunnerCtx,
    id: agents::AgentId,
    spec: agents::AgentSpec,
    update_tx: mpsc::UnboundedSender<agents::AgentUpdate>,
) {
    let _ = update_tx.send(agents::AgentUpdate::TranscriptAppend {
        id,
        message: lutin_llm::Message::User(spec.initial_prompt.clone()),
    });
    let mut agent = match build_subagent(&ctx, spec, id) {
        Ok(a) => a,
        Err(reason) => {
            let _ = update_tx.send(agents::AgentUpdate::Failed { id, error: reason });
            return;
        }
    };
    let mut stream = match agent.start() {
        Ok(s) => s,
        Err(e) => {
            let _ = update_tx.send(agents::AgentUpdate::Failed {
                id,
                error: format!("start: {e}"),
            });
            return;
        }
    };
    use futures_util::StreamExt;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::AssistantText(s) => {
                let _ = update_tx.send(agents::AgentUpdate::Progress {
                    id,
                    last_text: s,
                });
            }
            AgentEvent::AssistantMessage(msg) => {
                let _ = update_tx.send(agents::AgentUpdate::TranscriptAppend {
                    id,
                    message: msg,
                });
            }
            AgentEvent::ToolCallCompleted { call, outcome } => {
                let content = match outcome {
                    ToolResult::Ok(c) => c,
                    ToolResult::Err(e) => lutin_llm::ToolResultContent {
                        call_id: call.id.clone(),
                        content: format!("{e}"),
                        is_error: true,
                    },
                    other => lutin_llm::ToolResultContent {
                        call_id: call.id.clone(),
                        content: format!("{other:?}"),
                        is_error: true,
                    },
                };
                let _ = update_tx.send(agents::AgentUpdate::TranscriptAppend {
                    id,
                    message: lutin_llm::Message::ToolResult(content),
                });
            }
            AgentEvent::Error(e) => {
                let _ = update_tx.send(agents::AgentUpdate::Failed {
                    id,
                    error: format!("{e}"),
                });
                return;
            }
            AgentEvent::Finished(_) => break,
            _ => {}
        }
    }
    let outcome = agent.join().await;
    match outcome.finish_reason {
        AgentFinishReason::Stopped | AgentFinishReason::MaxRounds => {
            let final_text = outcome
                .last_assistant
                .as_ref()
                .and_then(|m| match m {
                    lutin_llm::Message::Assistant { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let _ = update_tx.send(agents::AgentUpdate::Completed {
                id,
                outcome: agents::AgentOutcome { final_text },
            });
        }
        other => {
            let error = match map_finish_reason(other) {
                FinishReason::Failed(reason) => reason,
                FinishReason::Cancelled => "cancelled".into(),
                FinishReason::Completed => "completed (unreachable)".into(),
                FinishReason::MaxRounds => "max rounds (unreachable)".into(),
            };
            let _ = update_tx.send(agents::AgentUpdate::Failed { id, error });
        }
    }
}

/// Build a sub-agent from an [`agents::AgentSpec`]. The persona inside
/// the spec was already resolved at the `SpawnAgent` tool boundary, so
/// there's no second persona load here — only `Settings` + sandbox
/// derivation. The initial user prompt is queued so the caller's
/// `agent.start()` consumes it on the first round.
fn build_subagent(
    ctx: &RunnerCtx,
    spec: agents::AgentSpec,
    owner_id: agents::AgentId,
) -> Result<Agent, String> {
    let agents::AgentSpec { initial_prompt, persona, parent_id: _ } = spec;
    let resolved = agent_build::resolve_args(ctx, Some(persona))
        .map_err(|e| format!("resolve args: {e}"))?;
    let build_args =
        resolved.as_build_args_with(ctx, PromptExtras::default(), Some(owner_id), None);
    let mut agent = lutin_workflow_sdk::agent::build_agent(build_args)
        .map_err(|e| format!("build agent: {}", agent_build::map_build_error(e)))?;
    agent
        .push_message(lutin_llm::Message::User(initial_prompt))
        .map_err(|e| format!("push initial prompt: {e}"))?;
    Ok(agent)
}

/// Derive `PromptExtras` for one chat turn so the SDK can substitute
/// `%message_count%`, `%user_message%`, `%agents:attached%`, etc. in
/// the persona's system prompt. Pulls live data from the on-disk
/// transcript and the sub-agent registry, plus a project-then-global
/// persona listing for `%personas:all%`.
pub(crate) async fn build_prompt_extras(
    ctx: &RunnerCtx,
    entries: &[Entry],
    current_persona: &Persona,
) -> PromptExtras {
    let user_message = entries.iter().rev().find_map(|e| match &e.message {
        lutin_llm::Message::User(t) => Some(t.clone()),
        _ => None,
    });
    let latest_response = entries.iter().rev().find_map(|e| match &e.message {
        lutin_llm::Message::Assistant { text, .. } if !text.is_empty() => Some(text.clone()),
        _ => None,
    });

    let attached_agents: Vec<PromptAgentEntry> = fetch_summaries(ctx)
        .await
        .into_iter()
        .map(|sum| PromptAgentEntry {
            name: sum.id.to_string(),
            status: status_label(&sum.status).to_owned(),
        })
        .collect();

    let personas = Persona::list(&ctx.resolver)
        .map(|all| {
            all.into_iter()
                .filter(|p| p.name != current_persona.name)
                .map(|p| PromptPersonaEntry {
                    name: p.name,
                    display_name: p.display_name,
                    description: p.description,
                })
                .collect()
        })
        .unwrap_or_default();

    PromptExtras {
        message_count: entries.len(),
        user_message,
        latest_response,
        attached_agents,
        personas,
        chat_kind: "main".into(),
        ..PromptExtras::default()
    }
}

/// One round-trip to the registry actor. Returns an empty vec when
/// the cmd channel is closed or the reply is dropped — both mean "no
/// children visible" from the caller's POV.
pub(crate) async fn fetch_summaries(ctx: &RunnerCtx) -> Vec<agents::AgentSummary> {
    let (tx, rx) = oneshot::channel();
    if ctx
        .agent_registry
        .send(agents::AgentRegistryCmd::Snapshot { reply: tx })
        .is_err()
    {
        return Vec::new();
    }
    rx.await.unwrap_or_default()
}

/// Project an `AgentSummary` to the chat protocol's `SubAgentInfo` wire
/// shape. Pure — caller wraps in `Vec` if it's snapshotting many.
pub(crate) fn project_summary(s: agents::AgentSummary) -> SubAgentInfo {
    SubAgentInfo {
        id: s.id.to_string(),
        parent_id: s.parent_id.map(|p| p.to_string()),
        persona: s.persona,
        status: match s.status {
            agents::AgentStatus::Running => SubAgentStatus::Running,
            agents::AgentStatus::Completed => SubAgentStatus::Completed,
            agents::AgentStatus::Failed { reason } => SubAgentStatus::Failed { reason },
            agents::AgentStatus::Stopped => SubAgentStatus::Stopped,
        },
        last_progress: s.last_progress,
    }
}

/// Snapshot one child's transcript and emit `SubAgentTranscriptUpdated`.
async fn broadcast_subagent_transcript(ctx: &RunnerCtx, id: agents::AgentId) {
    let (tx, rx) = oneshot::channel();
    if ctx
        .agent_registry
        .send(agents::AgentRegistryCmd::Transcript { id, reply: tx })
        .is_err()
    {
        return;
    }
    let messages = match rx.await {
        Ok(opt) => opt.unwrap_or_default(),
        Err(_) => {
            warn!(%id, "registry dropped Transcript reply");
            return;
        }
    };
    let history = project_messages(messages.iter());
    let _ = ctx.events.send(ChatEvent::SubAgentTranscriptUpdated {
        id: id.to_string(),
        history,
    });
}

/// Snapshot+broadcast helper: emit `SubAgentsChanged` on the engine's
/// event channel.
pub(crate) async fn broadcast_subagents(ctx: &RunnerCtx) {
    let snap = fetch_summaries(ctx).await.into_iter().map(project_summary).collect();
    let _ = ctx.events.send(ChatEvent::SubAgentsChanged(snap));
}

/// Render the `<active_subagents>` block injected into the
/// orchestrator's system prompt. `None` when the registry is empty or
/// unreachable.
pub(crate) async fn subagent_block(ctx: &RunnerCtx) -> Option<String> {
    let summaries = fetch_summaries(ctx).await;
    if summaries.is_empty() {
        return None;
    }
    let mut out = String::from("<active_subagents>\n");
    for s in &summaries {
        out.push_str("- ");
        out.push_str(&s.id.to_string());
        out.push_str(" status=");
        out.push_str(status_label(&s.status));
        if let agents::AgentStatus::Failed { reason } = &s.status {
            out.push_str(&format!(" reason={reason:?}"));
        }
        if let Some(p) = &s.last_progress {
            out.push_str(&format!(" progress={p:?}"));
        }
        out.push('\n');
    }
    out.push_str("</active_subagents>");
    Some(out)
}

fn status_label(status: &agents::AgentStatus) -> &'static str {
    match status {
        agents::AgentStatus::Running => "running",
        agents::AgentStatus::Completed => "completed",
        agents::AgentStatus::Failed { .. } => "failed",
        agents::AgentStatus::Stopped => "stopped",
    }
}

/// Append a sub-agent's terminal result to the parent transcript and
/// kick an auto-turn so the parent's LLM gets to react. Runs from the
/// runner's outer `select!` only — never from inside `run_turn`.
pub(crate) async fn handle_subagent_completion(
    ctx: &RunnerCtx,
    rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
    agent: &mut Option<Agent>,
    entries: &mut Vec<Entry>,
    review_session: &mut Option<review::ReviewSession>,
    evt: agents::CompletionEvent,
) {
    if let agents::CompletionEvent::TranscriptAppend { id, .. } = &evt {
        broadcast_subagent_transcript(ctx, *id).await;
        return;
    }
    let turn = ctx.next_turn();
    broadcast_subagents(ctx).await;
    let Some(a) = ensure_agent(agent, ctx, entries, turn) else {
        return;
    };
    let msg = match &evt {
        agents::CompletionEvent::Completed { id, outcome } => lutin_llm::Message::SubAgentReply {
            agent_id: id.to_string(),
            text: outcome.final_text.clone(),
        },
        agents::CompletionEvent::Failed { id, error } => lutin_llm::Message::SubAgentFailure {
            agent_id: id.to_string(),
            reason: error.clone(),
        },
        agents::CompletionEvent::TranscriptAppend { .. } => unreachable!(),
    };
    if let Err(e) = a.push_message(msg.clone()) {
        warn!(error = %e, "push agent response failed; skipping auto-turn");
        return;
    }
    entries.push(Entry {
        message: msg,
        metrics: MessageMetrics {
            timestamp: Some(now_rfc3339()),
            ..Default::default()
        },
    });
    if let Err(e) = store::save(&ctx.state_dir, entries) {
        warn!(error = %e, "save transcript after agent response failed");
    }
    write_summary(&ctx.state_dir, &ctx.resolver, entries);
    let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    run_turn(ctx, rx, a, entries, review_session, None, turn).await;
}
