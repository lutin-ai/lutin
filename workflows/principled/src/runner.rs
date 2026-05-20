//! Singleton agent runner task.
//!
//! Owns the `Agent` for the lifetime of one turn. Reads commands off
//! `mpsc::UnboundedReceiver` in two states:
//!
//! * Idle — `recv().await` for the next command. `Cancel` while idle
//!   is a no-op (cancellation has nothing to act on).
//! * Running — concurrently selects on the agent's event stream and
//!   the same command channel. `Cancel` calls `agent.cancel()`;
//!   further `Send` commands stay buffered in the channel and are
//!   picked up by the next idle iteration. No locks, no shared
//!   `Agent`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lutin_agent_sdk::Agent;
use lutin_storage::Resolver;
use lutin_workflow_sdk::agent::build_agent as sdk_build_agent;
use principled::{ChatError, ChatEvent, FinishReason, TurnId};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::warn;

use crate::agent_build::{map_build_error, resolve_args};
use crate::agents;
use crate::mutation::{MutateOp, apply_mutation};
use crate::projection::write_summary;
use crate::review;
use crate::store::{self, Entry};
use crate::subagents_glue::handle_subagent_completion;
use crate::turn::run_turn;

/// Commands the WS handlers send to the singleton agent runner task.
/// `Cancel` interrupts an in-flight turn (and is a no-op when idle);
/// `Send` enqueues a new turn (queued behind any in-flight one).
pub(crate) enum AgentCmd {
    Send { text: String, turn: TurnId },
    Rerun { turn: TurnId },
    Cancel,
    /// In-place mutation of the transcript. The new state is delivered
    /// via the `HistoryReplaced` broadcast (single source of truth for
    /// every subscriber, including the originator); the `reply` here
    /// just carries success/failure for the request/response pair.
    Mutate {
        op: MutateOp,
        reply: tokio::sync::oneshot::Sender<Result<(), ChatError>>,
    },
}

#[derive(Clone)]
pub(crate) struct RunnerCtx {
    pub(crate) state_dir: PathBuf,
    pub(crate) project_config_dir: PathBuf,
    /// Resolver over the global + project config dirs. Built once at
    /// startup and shared via `Arc` so per-call clones don't fan out
    /// `PathBuf`s. Anything reading personas / settings goes through
    /// here rather than re-constructing.
    pub(crate) resolver: Arc<Resolver>,
    pub(crate) events: broadcast::Sender<ChatEvent>,
    /// Shared with `AppState`. Runner writes the failure reason here
    /// on exit; readers consult the latest published value via the
    /// watch channel without taking a lock.
    pub(crate) failure: watch::Sender<Option<String>>,
    /// Shared with `AppState`. Runner allocates fresh `TurnId`s for
    /// auto-turns triggered by sub-agent completions, drawing from the
    /// same monotonic source as user-driven turns so ids stay unique.
    pub(crate) next_turn: Arc<AtomicU64>,
    /// Sender into the sub-agent registry actor. Held here (not just
    /// in `AppState`) because the spawner closure clones a `RunnerCtx`
    /// into each child task.
    pub(crate) agent_registry: mpsc::UnboundedSender<agents::AgentRegistryCmd>,
}

impl RunnerCtx {
    pub(crate) fn next_turn(&self) -> TurnId {
        TurnId(self.next_turn.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn record_failure(&self, reason: impl Into<String>) {
        let reason = reason.into();
        warn!(error = %reason, "agent runner bailing");
        self.failure.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(reason);
                true
            } else {
                false
            }
        });
    }
}

pub(crate) async fn run_agent_loop(
    ctx: RunnerCtx,
    mut rx: mpsc::UnboundedReceiver<AgentCmd>,
    mut completions_rx: mpsc::UnboundedReceiver<agents::CompletionEvent>,
) {
    let mut entries: Vec<Entry> = match store::load(&ctx.state_dir) {
        Ok(es) => es,
        Err(e) => {
            ctx.record_failure(format!("load transcript: {e}"));
            return;
        }
    };
    write_summary(&ctx.state_dir, &ctx.resolver, &entries);

    let mut agent: Option<Agent> = None;
    let mut review_session: Option<review::ReviewSession> = None;
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCmd::Send { text, turn } => {
                        if let Some(a) = ensure_agent(&mut agent, &ctx, &entries, turn) {
                            run_turn(
                                &ctx, &mut rx, a, &mut entries, &mut review_session,
                                Some(text), turn,
                            ).await;
                        }
                    }
                    AgentCmd::Rerun { turn } => {
                        if let Some(a) = ensure_agent(&mut agent, &ctx, &entries, turn) {
                            run_turn(
                                &ctx, &mut rx, a, &mut entries, &mut review_session,
                                None, turn,
                            ).await;
                        }
                    }
                    AgentCmd::Cancel => {}
                    AgentCmd::Mutate { op, reply } => {
                        let active = review_session
                            .as_ref()
                            .is_some_and(|s| s.has_active_frame());
                        if active {
                            let _ = reply.send(Err(ChatError::ReviewInFlight));
                        } else {
                            let result = apply_mutation(&ctx, agent.as_mut(), &mut entries, op);
                            let _ = reply.send(result);
                        }
                    }
                }
            }
            evt = completions_rx.recv() => match evt {
                Some(evt) => {
                    handle_subagent_completion(
                        &ctx, &mut rx, &mut agent, &mut entries, &mut review_session, evt,
                    ).await;
                }
                None => {
                    // Registry actor is gone — only happens at shutdown
                    // when its `cmd_tx` is dropped. Treat as benign.
                }
            }
        }
    }
}

/// Lazy-build the agent on first use; surface init failures as a
/// turn-level error so the runner stays alive. Returns `None` when
/// the build failed (and the caller should skip the turn).
pub(crate) fn ensure_agent<'a>(
    slot: &'a mut Option<Agent>,
    ctx: &RunnerCtx,
    entries: &[Entry],
    turn: TurnId,
) -> Option<&'a mut Agent> {
    if slot.is_none() {
        match build_initial_agent(ctx, entries) {
            Ok(a) => *slot = Some(a),
            Err(reason) => {
                let _ = ctx.events.send(ChatEvent::MessageFinished {
                    turn_id: turn,
                    reason: FinishReason::Failed(reason),
                });
                return None;
            }
        }
    }
    slot.as_mut()
}

/// Build the agent on first use, seeding it from the in-memory entries
/// (which were loaded once at runner start and stay authoritative).
fn build_initial_agent(ctx: &RunnerCtx, entries: &[Entry]) -> Result<Agent, String> {
    let resolved = resolve_args(ctx, None).map_err(|e| format!("resolve args: {e}"))?;
    let mut agent = sdk_build_agent(resolved.as_build_args(ctx))
        .map_err(|e| format!("build agent: {}", map_build_error(e)))?;
    let messages = store::messages(entries);
    agent
        .edit_messages(|m| *m = messages)
        .map_err(|e| format!("seed agent messages: {e}"))?;
    Ok(agent)
}
