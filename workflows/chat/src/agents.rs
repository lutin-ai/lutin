//! In-workflow sub-agent registry actor.
//!
//! Owns the live set of spawned sub-agents (`AgentSlot`s) for one chat
//! engine. Receives commands from the engine + tools (`AgentRegistryCmd`)
//! and updates from the spawned tasks themselves (`AgentUpdate`); pushes
//! terminal results out as `CompletionEvent`s on a dedicated channel the
//! engine drains in its main loop.
//!
//! State-machine rules (kept here so the bug lives next to the code that
//! has to enforce them):
//!
//! - `Stop { id }` synchronously sets `slot.status = Stopped` and fires
//!   `slot.abort.abort()`. Replies on its `oneshot` immediately.
//! - The aborted spawn future is cancelled mid-execution; it does **not**
//!   send `Completed`/`Failed` upstream.
//! - Late `AgentUpdate::Progress` from an aborted task may already be
//!   sitting in `update_rx`. The update handler **ignores updates whose
//!   slot is not in `Running`** — never resurrect or transition out of a
//!   terminal state.
//! - `Completed` / `Failed` updates also forward a `CompletionEvent` to
//!   the chat engine; `Stop` does **not** — the parent already knows it
//!   stopped its own child.
//!
//! Concurrency: all `AgentSlot`s live in the actor's `HashMap`. Nothing
//! is shared mutably across tasks. Spawned tasks only hold an
//! `mpsc::UnboundedSender<AgentUpdate>` clone (mpsc is the channel; the
//! registry is the single owner of slot state).

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use lutin_entities::Persona;
use lutin_llm::Message;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

/// Sub-agent identifier. Monotonic per-registry counter; not globally
/// unique. Display formats as `agent#<n>` to make orchestrator-facing
/// output (system prompt, tool replies) self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentId(pub u64);

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent#{}", self.0)
    }
}

/// Parse `agent#7` or the bare `7`. The protocol layer accepts both
/// (LLM tool args sometimes drop the prefix; the UI always emits it),
/// so the rule lives on the type once and every call site shares it.
impl FromStr for AgentId {
    type Err = AgentIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let body = s.strip_prefix("agent#").unwrap_or(s);
        body.parse::<u64>()
            .map(AgentId)
            .map_err(|_| AgentIdParseError(s.to_owned()))
    }
}

#[derive(Debug)]
pub struct AgentIdParseError(pub String);

impl fmt::Display for AgentIdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unparseable agent id: {:?}", self.0)
    }
}

impl std::error::Error for AgentIdParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Completed,
    Failed { reason: String },
    Stopped,
}

/// Inputs to spawn one sub-agent. `persona` is the already-loaded
/// `Persona` — validation happens at the SpawnAgent tool boundary, so
/// by the time a spec lands in the registry the persona is guaranteed
/// to exist. The spawner consumes the spec; the registry only retains
/// the persona's display name in the slot for the UI panel.
pub struct AgentSpec {
    pub initial_prompt: String,
    pub persona: Persona,
    /// `None` when the spawn comes from the main session's orchestrator;
    /// `Some(id)` when one sub-agent spawned another. Threaded into
    /// `AgentSlot.parent_id` so the UI can render the family tree.
    pub parent_id: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutcome {
    pub final_text: String,
}

#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub id: AgentId,
    pub parent_id: Option<AgentId>,
    pub persona: String,
    pub status: AgentStatus,
    pub last_progress: Option<String>,
}

pub enum AgentRegistryCmd {
    Spawn {
        spec: AgentSpec,
        reply: oneshot::Sender<AgentId>,
    },
    Status {
        id: AgentId,
        reply: oneshot::Sender<Option<AgentStatus>>,
    },
    Stop {
        id: AgentId,
        reply: oneshot::Sender<()>,
    },
    Snapshot {
        reply: oneshot::Sender<Vec<AgentSummary>>,
    },
    /// Read-only transcript fetch for one slot. Replies with `None`
    /// when the id is unknown so callers can distinguish "registry
    /// gone" (`send` fails on the cmd channel) from "no such agent"
    /// (oneshot replies with `None`).
    Transcript {
        id: AgentId,
        reply: oneshot::Sender<Option<Vec<Message>>>,
    },
}

pub enum AgentUpdate {
    Progress { id: AgentId, last_text: String },
    Completed { id: AgentId, outcome: AgentOutcome },
    Failed { id: AgentId, error: String },
    /// One new message landed in the child's transcript. Pushed by the
    /// child task whenever the SDK appends to its message vec —
    /// assistant turns, tool exchanges, sub-agent replies. The
    /// registry mirrors `message` into the slot's transcript and the
    /// engine re-fetches via `Transcript` cmd to broadcast.
    TranscriptAppend { id: AgentId, message: Message },
}

/// Forwarded to the chat engine by the registry on terminal child
/// transitions (Completed / Failed) **and** on transcript appends. The
/// engine appends an agent-response message to its own transcript on
/// terminals and broadcasts `SubAgentTranscriptUpdated` on appends so
/// open child-transcript views stream live. Stop never produces a
/// `CompletionEvent` — see top-of-file rules.
#[derive(Debug, Clone)]
pub enum CompletionEvent {
    Completed { id: AgentId, outcome: AgentOutcome },
    Failed { id: AgentId, error: String },
    /// Non-terminal signal: "this child's transcript grew". The
    /// payload isn't carried here — the engine re-fetches via the
    /// `Transcript` cmd so the broadcast always reflects the
    /// registry's authoritative slot, not a possibly-stale message
    /// snapshot.
    TranscriptAppend { id: AgentId },
}

const PROGRESS_MAX_CHARS: usize = 200;

/// One live or terminal slot. The variant *is* the state — `Completed`
/// always carries its outcome, `Failed` always carries its reason, and
/// `Running` is the only variant that owns an `AbortHandle`. This makes
/// "Completed without an outcome" or "post-terminal abort" unrepresentable
/// rather than an invariant policed by comments.
struct AgentSlot {
    persona: String,
    parent_id: Option<AgentId>,
    /// Append-only mirror of the child agent's `messages`. Pushed into
    /// by `AgentUpdate::TranscriptAppend` events; never trimmed because
    /// the read-only UI panel may scroll back to first turn.
    transcript: Vec<Message>,
    state: AgentSlotState,
}

enum AgentSlotState {
    Running {
        progress: Option<String>,
        abort: AbortHandle,
    },
    Completed {
        progress: Option<String>,
    },
    Failed {
        reason: String,
        progress: Option<String>,
    },
    Stopped {
        progress: Option<String>,
    },
}

impl AgentSlot {
    fn status(&self) -> AgentStatus {
        match &self.state {
            AgentSlotState::Running { .. } => AgentStatus::Running,
            AgentSlotState::Completed { .. } => AgentStatus::Completed,
            AgentSlotState::Failed { reason, .. } => {
                AgentStatus::Failed { reason: reason.clone() }
            }
            AgentSlotState::Stopped { .. } => AgentStatus::Stopped,
        }
    }

    fn progress(&self) -> Option<&str> {
        match &self.state {
            AgentSlotState::Running { progress, .. }
            | AgentSlotState::Completed { progress, .. }
            | AgentSlotState::Failed { progress, .. }
            | AgentSlotState::Stopped { progress } => progress.as_deref(),
        }
    }
}

/// Pluggable child-spawner so the actor doesn't hard-depend on
/// `lutin_agent_sdk::Agent` construction. Production wiring builds a
/// real `Agent`, drives its event stream, and translates events to
/// `AgentUpdate`s; tests pass a stub that fabricates updates directly.
///
/// Returning `AbortHandle` lets the actor cancel the task on `Stop` /
/// engine shutdown without keeping a `JoinHandle` around.
pub type Spawner = Box<
    dyn FnMut(AgentId, AgentSpec, mpsc::UnboundedSender<AgentUpdate>) -> AbortHandle + Send,
>;

#[allow(dead_code)] // engine uses `spawn_with_channels` directly; this is a test convenience.
pub struct RegistryHandles {
    pub cmd_tx: mpsc::UnboundedSender<AgentRegistryCmd>,
    pub completions_rx: mpsc::UnboundedReceiver<CompletionEvent>,
}

pub struct Registry {
    cmd_rx: mpsc::UnboundedReceiver<AgentRegistryCmd>,
    update_tx: mpsc::UnboundedSender<AgentUpdate>,
    update_rx: mpsc::UnboundedReceiver<AgentUpdate>,
    completions: mpsc::UnboundedSender<CompletionEvent>,
    spawner: Spawner,
    slots: HashMap<AgentId, AgentSlot>,
    next_id: u64,
}

impl Registry {
    /// Build the registry, spawn its actor task, and return the engine
    /// handles. Dropping `cmd_tx` closes `cmd_rx`, the actor exits its
    /// loop, and any live children are aborted on the way out.
    ///
    /// Tests use this; the production path uses
    /// [`Registry::spawn_with_channels`] so the engine can hold the
    /// command sender before the spawner closure is built (the spawner
    /// captures a `RunnerCtx` that itself carries the sender — a cycle
    /// the closure-on-construction API can't express).
    #[allow(dead_code)] // test-only convenience; production uses `spawn_with_channels`.
    pub fn spawn(spawner: Spawner) -> RegistryHandles {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (completions_tx, completions_rx) = mpsc::unbounded_channel();
        Self::spawn_with_channels(cmd_rx, completions_tx, spawner);
        RegistryHandles { cmd_tx, completions_rx }
    }

    /// Construct + spawn the registry actor consuming caller-provided
    /// command + completion channel ends. The caller keeps the matching
    /// `cmd_tx` and `completions_rx`. Use this when callers need to
    /// thread `cmd_tx` into context that the spawner closure captures
    /// (e.g. a `RunnerCtx` that the spawner clones into each child).
    pub fn spawn_with_channels(
        cmd_rx: mpsc::UnboundedReceiver<AgentRegistryCmd>,
        completions: mpsc::UnboundedSender<CompletionEvent>,
        spawner: Spawner,
    ) {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let registry = Registry {
            cmd_rx,
            update_tx,
            update_rx,
            completions,
            spawner,
            slots: HashMap::new(),
            next_id: 1,
        };
        tokio::spawn(registry.run());
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(c) => self.handle_cmd(c),
                    None => break,
                },
                update = self.update_rx.recv() => {
                    // self holds update_tx, so the channel is open while
                    // the actor is alive.
                    let u = update.expect("update_rx closed while actor holds update_tx");
                    self.handle_update(u);
                }
            }
        }
        // Engine shutdown: actively reap any still-running children.
        for (_, slot) in self.slots.drain() {
            if let AgentSlotState::Running { abort, .. } = slot.state {
                abort.abort();
            }
        }
    }

    fn handle_cmd(&mut self, cmd: AgentRegistryCmd) {
        match cmd {
            AgentRegistryCmd::Spawn { spec, reply } => {
                let id = AgentId(self.next_id);
                self.next_id += 1;
                let persona = spec.persona.name.clone();
                let parent_id = spec.parent_id;
                let abort = (self.spawner)(id, spec, self.update_tx.clone());
                self.slots.insert(
                    id,
                    AgentSlot {
                        persona,
                        parent_id,
                        transcript: Vec::new(),
                        state: AgentSlotState::Running { progress: None, abort },
                    },
                );
                let _ = reply.send(id);
            }
            AgentRegistryCmd::Status { id, reply } => {
                let _ = reply.send(self.slots.get(&id).map(AgentSlot::status));
            }
            AgentRegistryCmd::Stop { id, reply } => {
                // Take + match + reinsert so the abort moves out of the
                // Running variant and the slot lands in `Stopped` with
                // its progress preserved. A non-Running slot is left
                // exactly as it was — Stop is a no-op past terminal.
                if let Some(slot) = self.slots.remove(&id) {
                    let AgentSlot { persona, parent_id, transcript, state } = slot;
                    let next_state = match state {
                        AgentSlotState::Running { abort, progress } => {
                            abort.abort();
                            AgentSlotState::Stopped { progress }
                        }
                        terminal => terminal,
                    };
                    self.slots.insert(
                        id,
                        AgentSlot { persona, parent_id, transcript, state: next_state },
                    );
                }
                let _ = reply.send(());
            }
            AgentRegistryCmd::Snapshot { reply } => {
                let mut summaries: Vec<AgentSummary> = self
                    .slots
                    .iter()
                    .map(|(id, s)| AgentSummary {
                        id: *id,
                        parent_id: s.parent_id,
                        persona: s.persona.clone(),
                        status: s.status(),
                        last_progress: s.progress().map(str::to_owned),
                    })
                    .collect();
                // Stable order by id so the system-prompt block doesn't
                // reshuffle every turn.
                summaries.sort_by_key(|s| s.id.0);
                let _ = reply.send(summaries);
            }
            AgentRegistryCmd::Transcript { id, reply } => {
                let _ = reply.send(self.slots.get(&id).map(|s| s.transcript.clone()));
            }
        }
    }

    fn handle_update(&mut self, update: AgentUpdate) {
        let id = match &update {
            AgentUpdate::Progress { id, .. }
            | AgentUpdate::Completed { id, .. }
            | AgentUpdate::Failed { id, .. }
            | AgentUpdate::TranscriptAppend { id, .. } => *id,
        };
        // Transcript appends are non-terminal and arrive frequently;
        // handle them up front so we don't pay the take/match/reinsert
        // round-trip on every assistant token batch. Forward to the
        // engine even if the slot has already gone terminal — a late
        // append from a still-flushing child is informative for the UI.
        if let AgentUpdate::TranscriptAppend { message, .. } = update {
            if let Some(slot) = self.slots.get_mut(&id) {
                slot.transcript.push(message);
            }
            let _ = self.completions.send(CompletionEvent::TranscriptAppend { id });
            return;
        }
        // Terminal-state guard: a `Stop` (or earlier Completed/Failed)
        // may have raced ahead of an already-queued update; only a
        // `Running` slot accepts further transitions. Take ownership
        // up front so the variant transition can move `progress` and
        // discard `abort` cleanly.
        let Some(slot) = self.slots.remove(&id) else {
            return;
        };
        let AgentSlot { persona, parent_id, transcript, state } = slot;
        let AgentSlotState::Running { progress, abort } = state else {
            self.slots.insert(
                id,
                AgentSlot { persona, parent_id, transcript, state },
            );
            return;
        };
        let next_state = match update {
            AgentUpdate::Progress { last_text, .. } => AgentSlotState::Running {
                progress: Some(truncate_chars(&last_text, PROGRESS_MAX_CHARS)),
                abort,
            },
            AgentUpdate::Completed { outcome, .. } => {
                // abort drops here — task already terminated, no need to fire.
                drop(abort);
                let _ = self.completions.send(CompletionEvent::Completed { id, outcome });
                AgentSlotState::Completed { progress }
            }
            AgentUpdate::Failed { error, .. } => {
                drop(abort);
                let _ = self.completions.send(CompletionEvent::Failed {
                    id,
                    error: error.clone(),
                });
                AgentSlotState::Failed { reason: error, progress }
            }
            // Already handled above; unreachable here.
            AgentUpdate::TranscriptAppend { .. } => unreachable!(),
        };
        self.slots
            .insert(id, AgentSlot { persona, parent_id, transcript, state: next_state });
    }
}

/// Truncate by character count (not byte count) so multi-byte UTF-8
/// sequences aren't sliced down the middle. Appends an ellipsis when a
/// cut occurs.
fn truncate_chars(s: &str, max: usize) -> String {
    let mut count = 0;
    let mut end = s.len();
    for (i, _) in s.char_indices() {
        if count == max {
            end = i;
            break;
        }
        count += 1;
    }
    if end < s.len() {
        let mut out = s[..end].to_owned();
        out.push('…');
        out
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    /// Test spawner that forwards each child's `update_tx` over a
    /// channel back to the test, letting the test send `AgentUpdate`s
    /// on the spawned task's behalf. The "child" task itself just
    /// sleeps until aborted. Channel-based instead of `Arc<Mutex<Vec>>`
    /// so the capture path uses the same actor primitives the
    /// production code does.
    fn capture_spawner() -> (Spawner, mpsc::UnboundedReceiver<mpsc::UnboundedSender<AgentUpdate>>) {
        let (capture_tx, capture_rx) = mpsc::unbounded_channel();
        let spawner: Spawner = Box::new(move |_id, _spec, tx| {
            // Send returns Err once the test drops `capture_rx`; tests
            // don't care since capture is best-effort scaffolding.
            let _ = capture_tx.send(tx);
            // Park the child until aborted; production replaces this
            // with an Agent::start() event-stream pump.
            let h = tokio::spawn(async {
                std::future::pending::<()>().await;
            });
            h.abort_handle()
        });
        (spawner, capture_rx)
    }

    fn make_spec() -> AgentSpec {
        AgentSpec {
            initial_prompt: "do the thing".into(),
            persona: Persona {
                name: "test".into(),
                ..Persona::default()
            },
            parent_id: None,
        }
    }

    async fn spawn_one(
        cmd_tx: &mpsc::UnboundedSender<AgentRegistryCmd>,
    ) -> AgentId {
        let (tx, rx) = oneshot::channel();
        cmd_tx
            .send(AgentRegistryCmd::Spawn { spec: make_spec(), reply: tx })
            .unwrap();
        rx.await.unwrap()
    }

    async fn status_of(
        cmd_tx: &mpsc::UnboundedSender<AgentRegistryCmd>,
        id: AgentId,
    ) -> Option<AgentStatus> {
        let (tx, rx) = oneshot::channel();
        cmd_tx.send(AgentRegistryCmd::Status { id, reply: tx }).unwrap();
        rx.await.unwrap()
    }

    async fn snapshot(
        cmd_tx: &mpsc::UnboundedSender<AgentRegistryCmd>,
    ) -> Vec<AgentSummary> {
        let (tx, rx) = oneshot::channel();
        cmd_tx.send(AgentRegistryCmd::Snapshot { reply: tx }).unwrap();
        rx.await.unwrap()
    }

    /// Settle the actor: round-trip a Snapshot to flush any queued
    /// updates the actor must process before the test reads state.
    async fn drain(cmd_tx: &mpsc::UnboundedSender<AgentRegistryCmd>) {
        let _ = snapshot(cmd_tx).await;
    }

    #[tokio::test]
    async fn spawn_returns_running_status() {
        let (spawner, _captured) = capture_spawner();
        let RegistryHandles { cmd_tx, .. } = Registry::spawn(spawner);
        let id = spawn_one(&cmd_tx).await;
        assert_eq!(status_of(&cmd_tx, id).await, Some(AgentStatus::Running));
        let snap = snapshot(&cmd_tx).await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, id);
        assert_eq!(snap[0].status, AgentStatus::Running);
    }

    #[tokio::test]
    async fn completion_emits_event_and_marks_slot() {
        let (spawner, mut captured) = capture_spawner();
        let RegistryHandles { cmd_tx, mut completions_rx } = Registry::spawn(spawner);
        let id = spawn_one(&cmd_tx).await;
        let tx = captured.recv().await.expect("captured update_tx");
        tx.send(AgentUpdate::Completed {
            id,
            outcome: AgentOutcome { final_text: "done".into() },
        })
        .unwrap();
        let evt = timeout(Duration::from_secs(1), completions_rx.recv())
            .await
            .expect("completion event")
            .expect("channel open");
        match evt {
            CompletionEvent::Completed { id: got, outcome } => {
                assert_eq!(got, id);
                assert_eq!(outcome.final_text, "done");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(status_of(&cmd_tx, id).await, Some(AgentStatus::Completed));
    }

    #[tokio::test]
    async fn failure_emits_event_and_marks_slot() {
        let (spawner, mut captured) = capture_spawner();
        let RegistryHandles { cmd_tx, mut completions_rx } = Registry::spawn(spawner);
        let id = spawn_one(&cmd_tx).await;
        let tx = captured.recv().await.expect("captured update_tx");
        tx.send(AgentUpdate::Failed { id, error: "boom".into() })
            .unwrap();
        let evt = timeout(Duration::from_secs(1), completions_rx.recv())
            .await
            .expect("completion event")
            .expect("channel open");
        match evt {
            CompletionEvent::Failed { id: got, error } => {
                assert_eq!(got, id);
                assert_eq!(error, "boom");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(
            status_of(&cmd_tx, id).await,
            Some(AgentStatus::Failed { reason: "boom".into() })
        );
    }

    #[tokio::test]
    async fn stop_marks_slot_and_emits_no_completion_event() {
        let (spawner, _captured) = capture_spawner();
        let RegistryHandles { cmd_tx, mut completions_rx } = Registry::spawn(spawner);
        let id = spawn_one(&cmd_tx).await;
        let (tx, rx) = oneshot::channel();
        cmd_tx.send(AgentRegistryCmd::Stop { id, reply: tx }).unwrap();
        rx.await.unwrap();
        assert_eq!(status_of(&cmd_tx, id).await, Some(AgentStatus::Stopped));
        // Give the loop a beat to (not) emit anything.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(completions_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn late_progress_after_stop_is_ignored() {
        let (spawner, mut captured) = capture_spawner();
        let RegistryHandles { cmd_tx, mut completions_rx } = Registry::spawn(spawner);
        let id = spawn_one(&cmd_tx).await;
        let tx = captured.recv().await.expect("captured update_tx");
        // Stop first.
        let (rtx, rrx) = oneshot::channel();
        cmd_tx.send(AgentRegistryCmd::Stop { id, reply: rtx }).unwrap();
        rrx.await.unwrap();
        // Now simulate a still-in-flight Progress, *and* a Completed —
        // both must be dropped on the floor.
        tx.send(AgentUpdate::Progress { id, last_text: "late".into() }).unwrap();
        tx.send(AgentUpdate::Completed {
            id,
            outcome: AgentOutcome { final_text: "ignored".into() },
        })
        .unwrap();
        drain(&cmd_tx).await;
        assert_eq!(status_of(&cmd_tx, id).await, Some(AgentStatus::Stopped));
        let snap = snapshot(&cmd_tx).await;
        assert_eq!(snap[0].last_progress, None);
        assert!(completions_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn progress_truncates_and_appears_in_snapshot() {
        let (spawner, mut captured) = capture_spawner();
        let RegistryHandles { cmd_tx, .. } = Registry::spawn(spawner);
        let id = spawn_one(&cmd_tx).await;
        let tx = captured.recv().await.expect("captured update_tx");
        let long: String = "x".repeat(PROGRESS_MAX_CHARS + 50);
        tx.send(AgentUpdate::Progress { id, last_text: long }).unwrap();
        drain(&cmd_tx).await;
        let snap = snapshot(&cmd_tx).await;
        let p = snap[0].last_progress.as_deref().unwrap();
        // Truncation appends an ellipsis (one extra char beyond max).
        assert!(p.ends_with('…'));
        assert_eq!(p.chars().count(), PROGRESS_MAX_CHARS + 1);
    }

    #[tokio::test]
    async fn unknown_id_status_is_none() {
        let (spawner, _captured) = capture_spawner();
        let RegistryHandles { cmd_tx, .. } = Registry::spawn(spawner);
        assert_eq!(status_of(&cmd_tx, AgentId(999)).await, None);
    }

    #[tokio::test]
    async fn dropping_handles_reaps_running_children() {
        let (spawner, mut captured) = capture_spawner();
        let RegistryHandles { cmd_tx, .. } = Registry::spawn(spawner);
        let _id = spawn_one(&cmd_tx).await;
        let tx = captured.recv().await.expect("captured update_tx");
        // Drop the only command sender → actor exits → drains slots and
        // aborts the captured tasks. We can't directly observe the
        // abort, but we can confirm the captured update_tx eventually
        // notices the receiver is gone.
        drop(cmd_tx);
        // Give the actor a tick to drain.
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if tx.send(AgentUpdate::Progress { id: AgentId(1), last_text: "x".into() }).is_err() {
                return;
            }
        }
        panic!("actor did not drop update_rx after cmd_tx was dropped");
    }
}
