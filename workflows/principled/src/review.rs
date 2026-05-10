//! Review-loop driver: an `ApprovalPolicy` that gates each tool call
//! through the configured set of single-principle reviewers.
//!
//! Pass-with-nit and unanimous pass → `Allow` (SDK runs the tool).
//! Any blocking failure → `Deny` (SDK feeds `tool_result(Denied)` back
//! and the agent retries; we re-enter `decide` for the new call).
//! `VerdictKind::Fail{ severity: Rethink, .. }` queues a rewind via
//! `rewind_tx`; the runner in `engine::run_turn` cancels the agent
//! and restores the prior frame.
//!
//! ## State ownership
//!
//! The step stack — the only mutable state — is owned by the runner
//! task as a `ReviewSession`. `ApprovalPolicy::decide`, called from
//! the SDK's task with only `&self`, talks to the runner over an
//! `mpsc<ReviewRequest>` channel and awaits replies on per-request
//! oneshots. No `Arc<Mutex<…>>` of state crosses task boundaries; the
//! runner is the single writer of the stack and of
//! `live_messages_len`.
//!
//! Pre-exec artifact for `edit`/`write` is simulated via
//! `simulate_artifact` and forwarded only to reviewers that opted into
//! `ContextItem::ToolArtifact`. File snapshots are captured by the
//! runner at frame push so a rewind can revert.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, StreamExt};
use lutin_agent_sdk::{Approval, ApprovalPolicy};
use lutin_entities::Persona;
use lutin_llm::{LlmProvider, ModelId, ToolCall};
use lutin_settings::Settings;
use lutin_storage::Resolver;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::timeout as tokio_timeout;
use tracing::warn;

/// Per-attempt timeout for a single reviewer LLM call. Long enough for
/// slow models, short enough that 3 attempts × 60s caps total wall time
/// at ~3 min before a principle is declared dead.
const REVIEWER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(60);
/// Total attempts (initial + retries) before a principle is treated as
/// a hard system failure. Retrying beyond this would just amplify load
/// against an already-struggling backend.
const MAX_REVIEWER_ATTEMPTS: u32 = 3;
/// Linear backoff between attempts: attempt N waits N * BASE before
/// retrying. Buffered fan-out already staggers in time, so no jitter.
const RETRY_BACKOFF_BASE: Duration = Duration::from_millis(200);

/// Signal carried over the rewind channel from the review-loop driver
/// to the engine's outer turn loop. `Continue` is the existing rethink
/// path: cancel the agent, restore the snapshot, restart the agent
/// stream. `Abort` halts the turn entirely after restoring — used when
/// the review system itself fails (e.g. a reviewer LLM is unreachable
/// after retries). The user must manually re-engage; we do not
/// auto-retry into a wedged backend.
#[derive(Debug, Clone)]
pub enum RewindSignal {
    Continue { feedback: String },
    Abort { reason: String },
}

use crate::principle::{ContextItem, OnMaxRetries, Principle};
use crate::reviewer::{review, ReviewInputs};
use crate::step::{
    AttemptOutcome, AttemptRecord, BlockingSeverity, ReviewerSlotStatus, StepFrame, StepId,
    StepStack, StepStatus, Verdict, VerdictKind,
};
use crate::store::now_rfc3339;
use principled::{
    ChatEvent, ReviewLogEntry, ReviewResolution, ReviewSeverityWire, ReviewVerdictWire,
};

/// Sentinel prepended to every denial reason this module emits. The
/// SDK wraps it as `denied: <REVIEW_DENY_TAG> {reason}` in the
/// `tool_result.content`. `engine::squash_denied_attempts` matches
/// the tag rather than the bare `denied:` prefix so genuine in-tool
/// errors aren't squashed.
pub(crate) const REVIEW_DENY_TAG: &str = "<review-deny>";

/// Name of the agent-self-issued rewind tool. Defined here (rather
/// than next to the tool implementation) because the approval policy
/// also needs the literal — exempting `abort_step` from the iteration
/// gate and naming it in the deny message that nudges the agent
/// toward the escape hatch.
pub const ABORT_STEP_TOOL_NAME: &str = "abort_step";

/// Prefix prepended to the agent's `reason` when `abort_step` ships it
/// as `RewindSignal::Continue` feedback. The runner detects this prefix
/// to distinguish self-aborts from reviewer-issued rewinds and to break
/// the consecutive-self-abort loop.
pub const SELF_ABORT_FEEDBACK_PREFIX: &str = "agent self-aborted: ";

fn deny_reason(message: impl Into<String>) -> String {
    format!("{REVIEW_DENY_TAG} {}", message.into())
}

/// True iff `tool_result_content` came from a review-loop denial.
pub(crate) fn is_review_denial(tool_result_content: &str) -> bool {
    tool_result_content
        .strip_prefix("denied: ")
        .is_some_and(|rest| rest.starts_with(REVIEW_DENY_TAG))
}

/// Reviewer assets resolved once at policy construction. The agent
/// side calls reviewer LLMs through `provider`; the runner side reads
/// `principle` to interpret verdicts (max_retries, on_max_retries,
/// title for messages). Shared via `Arc` so both halves point at the
/// same data.
pub struct ReviewerBundle {
    pub principle: Principle,
    pub persona: Persona,
    pub provider: Arc<dyn LlmProvider>,
    pub model: ModelId,
}

/// Request from the agent-side `ApprovalPolicy::decide` to the
/// runner-side `ReviewSession`. Each request carries a oneshot for
/// the runner's reply.
pub enum ReviewRequest {
    /// Push a new step frame (or note that one is already active) for
    /// this tool call. Captures the file snapshot in the runner so
    /// the writer of `StepStack` is one-and-only-one. The first
    /// `BeginFrame` of a step locks its `tool_name` as that step's
    /// iterated tool — see `StepFrame::iterated_tool`.
    BeginFrame {
        tool_name: String,
        principle_names: Vec<String>,
        snapshot_paths: Vec<PathBuf>,
        reply: oneshot::Sender<BeginOutcome>,
    },
    /// Apply a fan-out's verdicts to the active frame. Returns the
    /// final `Approval` after the failure-pick logic runs against
    /// the per-principle retry budgets.
    ApplyVerdicts {
        frame_id: StepId,
        call_id: String,
        tool_name: String,
        arguments_json: String,
        verdicts: Vec<Verdict>,
        reply: oneshot::Sender<Approval>,
    },
    /// Read the active step's locked tool name (if any). The agent
    /// side queries this *before* the principle filter so it can
    /// short-circuit non-iterated tool calls with a clear deny —
    /// keeping the agent on-iteration even when compaction has
    /// summarized the prior context away.
    CheckLock {
        reply: oneshot::Sender<Option<String>>,
    },
}

pub struct BeginOutcome {
    pub frame_id: StepId,
    /// True iff this call pushed a fresh frame (rather than reusing an
    /// already-`Active` one for an in-progress retry). Drives the
    /// `ReviewFrameOpened` event so the chrome inserts exactly one
    /// placeholder per step.
    pub is_new: bool,
    /// Names of principles whose slot is `Skipped` on the active
    /// frame. `decide` filters them out before LLM fan-out.
    pub skipped_principles: Vec<String>,
}

/// Runner-owned half. Holds the persistent step stack and the
/// per-turn principle metadata needed to interpret verdicts.
/// Constructed via `build` once per turn that has principles
/// configured; `stack` is moved across turn boundaries by the runner.
pub struct ReviewSession {
    pub stack: StepStack,
    pub bundles: Vec<Arc<ReviewerBundle>>,
    pub rewind_tx: mpsc::UnboundedSender<RewindSignal>,
    /// Broadcast sink for sidebar-facing review events
    /// (`ReviewFrameProgress`, `ReviewFrameResolved`). The runner is
    /// the single emitter of these — `decide` emits the
    /// `Opened`/`Started`/`Completed` events that don't depend on
    /// stack mutations.
    pub events: broadcast::Sender<ChatEvent>,
}

impl ReviewSession {
    pub fn has_active_frame(&self) -> bool {
        self.stack
            .frames()
            .iter()
            .any(|f| matches!(f.status, StepStatus::Active))
    }

    /// Process one request from `decide`. The caller passes
    /// `live_messages_len` because that count lives with the runner
    /// (it's tracked from agent events, not from anything `decide`
    /// observes).
    pub fn handle(&mut self, req: ReviewRequest, live_messages_len: usize) {
        match req {
            ReviewRequest::BeginFrame {
                tool_name,
                principle_names,
                snapshot_paths,
                reply,
            } => {
                let outcome = self.begin_frame(
                    tool_name,
                    principle_names,
                    snapshot_paths,
                    live_messages_len,
                );
                let _ = reply.send(outcome);
            }
            ReviewRequest::ApplyVerdicts {
                frame_id,
                call_id,
                tool_name,
                arguments_json,
                verdicts,
                reply,
            } => {
                let approval = self.apply_verdicts(
                    frame_id,
                    &call_id,
                    &tool_name,
                    &arguments_json,
                    verdicts,
                );
                let _ = reply.send(approval);
            }
            ReviewRequest::CheckLock { reply } => {
                let lock = self
                    .stack
                    .frames()
                    .iter()
                    .rev()
                    .find(|f| matches!(f.status, StepStatus::Active))
                    .map(|f| f.iterated_tool.clone());
                let _ = reply.send(lock);
            }
        }
    }

    fn begin_frame(
        &mut self,
        tool_name: String,
        principle_names: Vec<String>,
        snapshot_paths: Vec<PathBuf>,
        live_messages_len: usize,
    ) -> BeginOutcome {
        let needs_new = match self.stack.active() {
            Some(f) => !matches!(f.status, StepStatus::Active),
            None => true,
        };
        let frame_id = if needs_new {
            let id = self.stack.next_id();
            let names: Vec<&str> = principle_names.iter().map(String::as_str).collect();
            // The iterated tool is set at construction — every frame
            // is born from a tool call, so the lock is intrinsic to
            // the frame's identity. The agent-side gate consults it
            // to short-circuit drift.
            let mut frame = StepFrame::new(id, live_messages_len, &names, tool_name);
            for path in &snapshot_paths {
                if let Err(e) = frame.snapshot.capture_file(path) {
                    warn!(path = %path.display(), error = %e,
                        "snapshot capture failed; rewind cannot restore this file");
                }
            }
            self.stack.push(frame);
            id
        } else {
            self.stack.active().expect("active checked above").id
        };
        let frame = self
            .stack
            .frames()
            .iter()
            .find(|f| f.id == frame_id)
            .expect("frame just pushed");
        let skipped_principles = frame
            .reviewers
            .iter()
            .filter(|(_, slot)| matches!(slot.status, ReviewerSlotStatus::Skipped { .. }))
            .map(|(name, _)| name.clone())
            .collect();
        BeginOutcome {
            frame_id,
            is_new: needs_new,
            skipped_principles,
        }
    }

    fn apply_verdicts(
        &mut self,
        frame_id: StepId,
        call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        verdicts: Vec<Verdict>,
    ) -> Approval {
        let frame = self
            .stack
            .frames_mut()
            .iter_mut()
            .find(|f| f.id == frame_id)
            .expect("frame still on stack");

        // Apply slot updates and collect blocking failures by reference.
        let mut failures: Vec<&Verdict> = Vec::new();
        for v in &verdicts {
            if let Some((_, slot)) = frame
                .reviewers
                .iter_mut()
                .find(|(n, _)| *n == v.principle_name)
            {
                slot.status = match &v.kind {
                    VerdictKind::Pass | VerdictKind::PassWithNit { .. } => {
                        ReviewerSlotStatus::Passing
                    }
                    VerdictKind::Fail { .. } => ReviewerSlotStatus::Pending,
                };
            }
            if v.is_blocking() {
                failures.push(v);
            }
        }

        // Rethink outranks Fix.
        if let Some(rethink) = failures.iter().find(|v| {
            matches!(
                v.kind,
                VerdictKind::Fail {
                    severity: BlockingSeverity::Rethink,
                    ..
                }
            )
        }) {
            let feedback = format_feedback(&self.bundles, rethink);
            push_attempt(frame, call_id, tool_name, arguments_json, &verdicts, AttemptOutcome::Rewound);
            frame.status = StepStatus::Abandoned;
            // Squash event must precede the resolved event: the UI's
            // group-by-stepId render keys off `ReviewFrameResolved` to
            // drop the iteration-box outline, and once the box is gone
            // any orphan denied bubbles inside it would briefly appear
            // ungrouped before the squash hits.
            emit_squashed(&self.events, frame);
            let _ = self.events.send(ChatEvent::ReviewFrameResolved {
                step_id: frame_id.0,
                call_id: call_id.to_string(),
                outcome: ReviewResolution::Rewound { feedback: feedback.clone() },
            });
            let _ = self.rewind_tx.send(RewindSignal::Continue {
                feedback: feedback.clone(),
            });
            return Approval::Deny(deny_reason(format!("rewind requested: {feedback}")).into());
        }

        if failures.is_empty() {
            push_attempt(frame, call_id, tool_name, arguments_json, &verdicts, AttemptOutcome::Executed);
            frame.status = StepStatus::Accepted;
            emit_squashed(&self.events, frame);
            let _ = self.events.send(ChatEvent::ReviewFrameResolved {
                step_id: frame_id.0,
                call_id: call_id.to_string(),
                outcome: ReviewResolution::Accepted,
            });
            return Approval::Allow;
        }

        match pick_failure_with_budget(&self.bundles, frame, &failures) {
            FailureChoice::Retry { reason, attempt, max_attempts } => {
                push_attempt(frame, call_id, tool_name, arguments_json, &verdicts, AttemptOutcome::DeniedRetry);
                let blocking: Vec<String> = failures
                    .iter()
                    .map(|v| v.principle_name.clone())
                    .collect();
                let _ = self.events.send(ChatEvent::ReviewFrameProgress {
                    step_id: frame_id.0,
                    call_id: call_id.to_string(),
                    attempt,
                    max_attempts,
                    blocking,
                });
                Approval::Deny(deny_reason(reason).into())
            }
            FailureChoice::AskUser(reason) => {
                push_attempt(frame, call_id, tool_name, arguments_json, &verdicts, AttemptOutcome::Escalated);
                frame.status = StepStatus::Accepted;
                emit_squashed(&self.events, frame);
                let _ = self.events.send(ChatEvent::ReviewFrameResolved {
                    step_id: frame_id.0,
                    call_id: call_id.to_string(),
                    outcome: ReviewResolution::Escalated { reason: reason.clone() },
                });
                Approval::Deny(deny_reason(reason).into())
            }
            FailureChoice::AllSkipped => {
                push_attempt(frame, call_id, tool_name, arguments_json, &verdicts, AttemptOutcome::Executed);
                frame.status = StepStatus::Accepted;
                emit_squashed(&self.events, frame);
                let _ = self.events.send(ChatEvent::ReviewFrameResolved {
                    step_id: frame_id.0,
                    call_id: call_id.to_string(),
                    outcome: ReviewResolution::Accepted,
                });
                Approval::Allow
            }
        }
    }
}

/// Emit `AttemptsSquashed` listing this frame's denied attempt
/// `call_id`s. Called when a frame transitions to a terminal status
/// (`Accepted` or `Abandoned`) so the UI can drop the in-flight tool
/// bubbles for the failed attempts immediately, instead of waiting
/// for end-of-turn `HistoryReplaced` to clean them up.
///
/// The accepted/escalated attempt's `call_id` is intentionally
/// excluded: only `DeniedRetry` and `Rewound` outcomes get squashed
/// from the projected transcript, so only those should disappear
/// from the live UI.
fn emit_squashed(events: &broadcast::Sender<ChatEvent>, frame: &StepFrame) {
    use crate::step::AttemptOutcome as AO;
    let call_ids: Vec<String> = frame
        .attempts
        .iter()
        .filter(|a| matches!(a.outcome, AO::DeniedRetry | AO::Rewound))
        .map(|a| a.call_id.clone())
        .collect();
    if call_ids.is_empty() {
        return;
    }
    let _ = events.send(ChatEvent::AttemptsSquashed { call_ids });
}

fn push_attempt(
    frame: &mut StepFrame,
    call_id: &str,
    tool_name: &str,
    arguments_json: &str,
    verdicts: &[Verdict],
    outcome: AttemptOutcome,
) {
    frame.attempts.push(AttemptRecord {
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
        arguments_json: arguments_json.to_string(),
        verdicts: verdicts.to_vec(),
        outcome,
    });
}

fn format_feedback(bundles: &[Arc<ReviewerBundle>], v: &Verdict) -> String {
    let title = bundles
        .iter()
        .find(|b| b.principle.name == v.principle_name)
        .map(|b| b.principle.title.as_str())
        .unwrap_or(&v.principle_name);
    let reasoning = v.reasoning().unwrap_or("");
    format!("'{title}': {reasoning}")
}

/// Walk principles in least-important-first order; pick the first
/// failing one whose retry budget hasn't been exhausted. Side
/// effects: bumps `attempts` on the chosen slot, marks slots
/// `Skipped` when their budget runs out (and consults
/// `on_max_retries` for the escalation policy).
fn pick_failure_with_budget(
    bundles: &[Arc<ReviewerBundle>],
    frame: &mut StepFrame,
    failures: &[&Verdict],
) -> FailureChoice {
    for i in 0..frame.reviewers.len() {
        let name = frame.reviewers[i].0.as_str();
        let Some(v) = failures.iter().find(|v| v.principle_name == name) else {
            continue;
        };
        let VerdictKind::Fail {
            reasoning,
            suggested_fix,
            ..
        } = &v.kind
        else {
            debug_assert!(false, "non-blocking verdict in failures slice");
            continue;
        };
        let bundle = bundles
            .iter()
            .find(|b| b.principle.name == name)
            .expect("verdict came from a configured principle");
        let slot = &mut frame.reviewers[i].1;
        slot.attempts = slot.attempts.saturating_add(1);
        if slot.attempts <= bundle.principle.max_retries {
            let suggested = suggested_fix
                .as_ref()
                .map(|s| format!(". Suggested: {s}"))
                .unwrap_or_default();
            return FailureChoice::Retry {
                reason: format!(
                    "rejected by '{}': {}{}",
                    bundle.principle.title, reasoning, suggested
                ),
                attempt: slot.attempts,
                max_attempts: bundle.principle.max_retries,
            };
        }
        slot.status = ReviewerSlotStatus::Skipped {
            resolution: bundle.principle.on_max_retries,
        };
        if matches!(bundle.principle.on_max_retries, OnMaxRetries::AskUser) {
            return FailureChoice::AskUser(format!(
                "principle '{}' exceeded max_retries ({}); session paused for user input. \
                 Last reasoning: {}",
                bundle.principle.name, bundle.principle.max_retries, reasoning
            ));
        }
        // Continue → keep walking the remaining failures.
    }
    FailureChoice::AllSkipped
}

enum FailureChoice {
    Retry {
        reason: String,
        attempt: u32,
        max_attempts: u32,
    },
    AskUser(String),
    AllSkipped,
}

/// Agent-side half. Holds the bundles for LLM fan-out and a sender
/// into the runner's request channel.
pub struct ReviewApproval {
    bundles: Vec<Arc<ReviewerBundle>>,
    req_tx: mpsc::UnboundedSender<ReviewRequest>,
    /// Maximum number of reviewer LLM calls in flight at once during
    /// fan-out. Bounded to keep a single shared LLM backend from being
    /// overwhelmed when many principles are configured. See
    /// `SessionState::review_concurrency` for how this is sourced.
    concurrency: usize,
    /// Sender for [`RewindSignal::Abort`] when the review system itself
    /// fails (a reviewer LLM exhausts retries). Held here — not just on
    /// the runner side — because the failure is detected in the
    /// approval task during fan-out, before any `ApplyVerdicts` request
    /// crosses to the runner.
    rewind_tx: mpsc::UnboundedSender<RewindSignal>,
    /// Broadcast sink for sidebar-facing events emitted from
    /// `decide` (frame opened + per-reviewer started/completed). The
    /// matching `ReviewSession` holds its own clone of the same channel
    /// for the runner-emitted progress/resolved events.
    events: broadcast::Sender<ChatEvent>,
    /// Monotonic counter for `reviewer_call_id` on `ReviewerStarted` /
    /// `ReviewerCompleted`. Each reviewer LLM invocation gets a fresh
    /// id even when the same principle is re-run in a later iteration.
    /// Distinct from the per-principle retry `attempt` carried on
    /// `ReviewFrameProgress` — that one bounds against `max_retries`,
    /// this one is just an audit-row id.
    next_reviewer_call_id: AtomicU64,
    /// Session state dir; reviewer audit log appended at
    /// `<state_dir>/reviews.jsonl`.
    state_dir: PathBuf,
}

/// Pre-build the policy + runner-side session for a non-empty
/// principle list. Caller must short-circuit when
/// `principle_names.is_empty()` and leave the agent on `AllowAll`.
///
/// Wiring:
/// - `req_tx` is held by the returned `ReviewApproval`; the matching
///   `req_rx` is the runner's responsibility.
/// - `rewind_tx` flows from the runner's review-loop to its rewind
///   handler, mirroring the existing rewind channel.
pub fn build(
    resolver: &Resolver,
    settings: &Settings,
    principle_names: &[String],
    req_tx: mpsc::UnboundedSender<ReviewRequest>,
    rewind_tx: mpsc::UnboundedSender<RewindSignal>,
    events: broadcast::Sender<ChatEvent>,
    state_dir: PathBuf,
    concurrency: usize,
) -> Result<(ReviewApproval, ReviewSession), BuildError> {
    debug_assert!(
        !principle_names.is_empty(),
        "review::build called with empty principle list — caller should short-circuit"
    );
    let mut bundles = Vec::with_capacity(principle_names.len());
    for name in principle_names {
        let principle = Principle::load(resolver, name).map_err(|e| BuildError::Principle {
            name: name.clone(),
            reason: e.to_string(),
        })?;
        let persona = Persona::load(resolver, &principle.persona).map_err(|e| {
            BuildError::Persona {
                principle: name.clone(),
                persona: principle.persona.clone(),
                reason: e.to_string(),
            }
        })?;
        let Some(provider_name) = persona.provider.as_deref() else {
            return Err(BuildError::PersonaMissingProvider {
                principle: name.clone(),
                persona: principle.persona.clone(),
            });
        };
        let Some(provider_cfg) = settings.providers.iter().find(|p| p.name == provider_name)
        else {
            return Err(BuildError::ProviderNotFound {
                principle: name.clone(),
                provider: provider_name.into(),
            });
        };
        let provider = lutin_workflow_sdk::agent::build_provider(provider_cfg).map_err(|e| {
            BuildError::ProviderBuild {
                principle: name.clone(),
                reason: format!("{e}"),
            }
        })?;
        let Some(model) = persona.model.clone() else {
            return Err(BuildError::PersonaMissingModel {
                principle: name.clone(),
                persona: principle.persona.clone(),
            });
        };
        bundles.push(Arc::new(ReviewerBundle {
            principle,
            persona,
            provider,
            model: ModelId::new(model),
        }));
    }
    let approval = ReviewApproval {
        bundles: bundles.clone(),
        req_tx,
        events: events.clone(),
        next_reviewer_call_id: AtomicU64::new(0),
        state_dir,
        concurrency: concurrency.max(1),
        rewind_tx: rewind_tx.clone(),
    };
    let session = ReviewSession {
        stack: StepStack::default(),
        bundles,
        rewind_tx,
        events,
    };
    Ok((approval, session))
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("load principle '{name}': {reason}")]
    Principle { name: String, reason: String },
    #[error("load reviewer persona '{persona}' for principle '{principle}': {reason}")]
    Persona {
        principle: String,
        persona: String,
        reason: String,
    },
    #[error("principle '{principle}' reviewer persona '{persona}' has no provider")]
    PersonaMissingProvider { principle: String, persona: String },
    #[error("principle '{principle}' reviewer persona '{persona}' has no model")]
    PersonaMissingModel { principle: String, persona: String },
    #[error("principle '{principle}' provider '{provider}' not configured")]
    ProviderNotFound { principle: String, provider: String },
    #[error("principle '{principle}' provider build failed: {reason}")]
    ProviderBuild { principle: String, reason: String },
}

#[async_trait]
impl ApprovalPolicy for ReviewApproval {
    async fn decide(&self, call: &ToolCall) -> Approval {
        // Iteration gate: if a step is currently Active and locked to
        // a tool, reject anything else (except `abort_step`, the
        // self-issued rewind escape). This runs *before* the principle
        // filter so unrelated tools that wouldn't otherwise summon a
        // reviewer still get short-circuited — that's where drift
        // tends to happen after compaction shortens the agent's
        // memory of the iteration.
        if call.name.as_str() != ABORT_STEP_TOOL_NAME {
            let (lock_tx, lock_rx) = oneshot::channel();
            if self
                .req_tx
                .send(ReviewRequest::CheckLock { reply: lock_tx })
                .is_err()
            {
                return Approval::Deny(
                    deny_reason("review runner unavailable").into(),
                );
            }
            if let Ok(Some(locked)) = lock_rx.await
                && locked != call.name.as_str()
            {
                return Approval::Deny(
                    format!(
                        "tool '{}' is not available during this review iteration on '{}'. \
                         Call '{}' with adjusted arguments, or call '{}' with a 'reason' \
                         describing what you need to reconsider — that rewinds the step \
                         and frees the toolset.",
                        call.name.as_str(),
                        locked,
                        locked,
                        ABORT_STEP_TOOL_NAME,
                    )
                    .into(),
                );
            }
        }

        // Filter by applies_to (no state).
        let matching: Vec<Arc<ReviewerBundle>> = self
            .bundles
            .iter()
            .filter(|b| {
                b.principle
                    .applies_to
                    .iter()
                    .any(|n| n == call.name.as_str())
            })
            .cloned()
            .collect();
        if matching.is_empty() {
            return Approval::Allow;
        }

        // Ask the runner to push or reuse a frame and tell us which
        // principles are already Skipped on it. The runner is the
        // single writer of the stack; the snapshot capture happens
        // there too so file IO doesn't cross the channel.
        let snapshot_paths = snapshot_paths_for(call);
        let principle_names: Vec<String> =
            matching.iter().map(|b| b.principle.name.clone()).collect();
        let (begin_tx, begin_rx) = oneshot::channel();
        if self
            .req_tx
            .send(ReviewRequest::BeginFrame {
                tool_name: call.name.as_str().to_string(),
                principle_names,
                snapshot_paths,
                reply: begin_tx,
            })
            .is_err()
        {
            // Runner is gone → policy failure should not silently
            // approve. Deny with a tagged reason; the agent will see
            // a tool_result(Denied) and stop.
            return Approval::Deny(
                deny_reason("review runner unavailable").into(),
            );
        }
        let begin = match begin_rx.await {
            Ok(b) => b,
            Err(_) => {
                return Approval::Deny(deny_reason("review runner dropped reply").into());
            }
        };

        let args_summary = summarize_args(&call.arguments);
        if begin.is_new {
            let _ = self.events.send(ChatEvent::ReviewFrameOpened {
                step_id: begin.frame_id.0,
                call_id: call.id.as_str().to_string(),
                tool_name: call.name.as_str().to_string(),
                args_summary: args_summary.clone(),
            });
        }

        let live: Vec<Arc<ReviewerBundle>> = matching
            .into_iter()
            .filter(|b| !begin.skipped_principles.contains(&b.principle.name))
            .collect();

        // Compute artifact only if some live reviewer wants it; pass
        // it through to those reviewers.
        let wants_artifact = live
            .iter()
            .any(|b| b.principle.context.contains(&ContextItem::ToolArtifact));
        let artifact: Option<String> = if wants_artifact {
            simulate_artifact(call)
        } else {
            None
        };

        // Reviewer infra failure handling: each reviewer gets up to
        // MAX_REVIEWER_ATTEMPTS tries with a per-attempt timeout. If
        // any principle exhausts its retries, the review *system* is
        // considered unhealthy — we cannot make a sound gating
        // decision, so we abort the turn rather than silently
        // dropping a verdict (principles are gates, not hints).
        let verdicts = match run_reviewers_parallel(
            &live,
            call,
            artifact.as_deref(),
            &self.events,
            &self.next_reviewer_call_id,
            begin.frame_id,
            &self.state_dir,
            &args_summary,
            self.concurrency,
        )
        .await
        {
            Ok(verdicts) => verdicts,
            Err(failure) => {
                let reason = format!(
                    "reviewer for principle '{}' failed after {} attempts: {}",
                    failure.principle, failure.attempts, failure.last_error,
                );
                warn!(error = %reason, "review system failure; aborting turn");
                let _ = self.events.send(ChatEvent::ReviewFrameResolved {
                    step_id: begin.frame_id.0,
                    call_id: call.id.as_str().to_string(),
                    outcome: ReviewResolution::ReviewSystemFailure {
                        principle: failure.principle.clone(),
                        error: reason.clone(),
                    },
                });
                let _ = self
                    .rewind_tx
                    .send(RewindSignal::Abort { reason: reason.clone() });
                return Approval::Deny(deny_reason(reason).into());
            }
        };

        let arguments_json = serde_json::to_string(&call.arguments).unwrap_or_default();
        let (apply_tx, apply_rx) = oneshot::channel();
        if self
            .req_tx
            .send(ReviewRequest::ApplyVerdicts {
                frame_id: begin.frame_id,
                call_id: call.id.as_str().to_string(),
                tool_name: call.name.as_str().to_string(),
                arguments_json,
                verdicts,
                reply: apply_tx,
            })
            .is_err()
        {
            return Approval::Deny(deny_reason("review runner unavailable").into());
        }
        match apply_rx.await {
            Ok(approval) => approval,
            Err(_) => Approval::Deny(deny_reason("review runner dropped reply").into()),
        }
    }
}

/// Files the proposed tool call would mutate. Currently scoped to
/// `edit`, `edit_lines`, and `write` — each takes a `path` argument and
/// reads/replaces on-disk content. Other tools yield an empty list.
fn snapshot_paths_for(call: &ToolCall) -> Vec<PathBuf> {
    let name = call.name.as_str();
    if name != "edit" && name != "edit_lines" && name != "write" {
        return Vec::new();
    }
    call.arguments
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| vec![PathBuf::from(s)])
        .unwrap_or_default()
}

/// First-error wins, recorded for the user-facing abort reason.
pub(crate) struct ReviewSystemFailure {
    pub principle: String,
    pub attempts: u32,
    pub last_error: String,
}

/// Run a single reviewer with retries and a per-attempt timeout. On
/// exhaustion, returns `Err` carrying the last error string — the
/// caller short-circuits the whole fan-out on the first such error.
///
/// Persists the audit log + emits `ReviewerCompleted` only on success;
/// on retry-exhausted failure, no audit row is written (the turn is
/// going to be aborted, so a "fail" entry would misrepresent what
/// happened — the reviewer never produced a verdict).
async fn run_reviewer_with_retries(
    bundle: &Arc<ReviewerBundle>,
    call: &ToolCall,
    artifact: Option<&str>,
    events: &broadcast::Sender<ChatEvent>,
    reviewer_call_id: u64,
    frame_id: StepId,
    state_dir: &Path,
    args_summary: &str,
) -> Result<Verdict, ReviewSystemFailure> {
    let principle_name = &bundle.principle.name;
    let artifact = if bundle.principle.context.contains(&ContextItem::ToolArtifact) {
        artifact
    } else {
        None
    };
    let _ = events.send(ChatEvent::ReviewerStarted {
        step_id: frame_id.0,
        call_id: call.id.as_str().to_string(),
        reviewer_call_id,
        principle: principle_name.clone(),
    });

    let mut last_error: String = String::new();
    for attempt in 1..=MAX_REVIEWER_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(RETRY_BACKOFF_BASE * (attempt - 1)).await;
        }
        let inputs = ReviewInputs {
            principle: &bundle.principle,
            call,
            artifact,
            chat: None,
            prior_steps: None,
        };
        let call_fut = review(&*bundle.provider, &bundle.model, &bundle.persona, inputs);
        match tokio_timeout(REVIEWER_ATTEMPT_TIMEOUT, call_fut).await {
            Ok(Ok(verdict)) => {
                let entry = ReviewLogEntry {
                    ts: now_rfc3339(),
                    step_id: frame_id.0,
                    reviewer_call_id,
                    principle: principle_name.clone(),
                    persona: Some(bundle.persona.name.clone()),
                    tool_name: call.name.as_str().to_string(),
                    args_summary: args_summary.to_string(),
                    verdict: to_wire_verdict(&verdict.kind),
                    call_id: Some(call.id.as_str().to_string()),
                };
                if append_review_log(state_dir, &entry).is_ok() {
                    let _ = events.send(ChatEvent::ReviewerCompleted {
                        step_id: entry.step_id,
                        // Read from `call` directly; the persisted log
                        // entry uses `Option<String>` for legacy-row
                        // back-compat, but the live event always knows
                        // its callId from the in-flight ToolCall.
                        call_id: call.id.as_str().to_string(),
                        reviewer_call_id: entry.reviewer_call_id,
                        principle: entry.principle.clone(),
                        verdict: entry.verdict.clone(),
                        ts: entry.ts.clone(),
                    });
                }
                return Ok(verdict);
            }
            Ok(Err(e)) => {
                last_error = format!("{e}");
                warn!(
                    principle = %principle_name,
                    attempt,
                    error = %e,
                    "reviewer call failed; will retry"
                );
            }
            Err(_elapsed) => {
                last_error = format!(
                    "timed out after {}s",
                    REVIEWER_ATTEMPT_TIMEOUT.as_secs()
                );
                warn!(
                    principle = %principle_name,
                    attempt,
                    "reviewer call timed out; will retry"
                );
            }
        }
    }
    Err(ReviewSystemFailure {
        principle: principle_name.clone(),
        attempts: MAX_REVIEWER_ATTEMPTS,
        last_error,
    })
}

async fn run_reviewers_parallel(
    bundles: &[Arc<ReviewerBundle>],
    call: &ToolCall,
    artifact: Option<&str>,
    events: &broadcast::Sender<ChatEvent>,
    next_reviewer_call_id: &AtomicU64,
    frame_id: StepId,
    state_dir: &Path,
    args_summary: &str,
    concurrency: usize,
) -> Result<Vec<Verdict>, ReviewSystemFailure> {
    // Pre-allocate ids so log/event ordering doesn't depend on the
    // order `buffer_unordered` happens to schedule futures in.
    // Relaxed: ids only need uniqueness, not happens-before with any
    // other write — the sidebar uses them purely as a join key.
    let tagged: Vec<(u64, Arc<ReviewerBundle>)> = bundles
        .iter()
        .map(|b| {
            (
                next_reviewer_call_id.fetch_add(1, Ordering::Relaxed),
                Arc::clone(b),
            )
        })
        .collect();

    let mut stream = stream::iter(tagged.into_iter().map(|(id, bundle)| {
        let events = events.clone();
        async move {
            run_reviewer_with_retries(
                &bundle,
                call,
                artifact,
                &events,
                id,
                frame_id,
                state_dir,
                args_summary,
            )
            .await
        }
    }))
    .buffer_unordered(concurrency.max(1));

    // Short-circuit on:
    //   1. the first principle that exhausts its retries — backend
    //      is wedged, abort the turn; or
    //   2. the first blocking verdict (Fix or Rethink) — `apply_verdicts`
    //      only ever forwards ONE failure to the agent (rethink-over-fix
    //      among those collected, then budget-aware pick among fixes),
    //      so finishing every reviewer just to pick one is wasted LLM
    //      spend. The agent fixes the first failure and we re-review on
    //      the next iteration; principles that hadn't reported yet get
    //      their next chance then.
    //
    // Trade-off: with concurrency > 1, a Fix that lands before a slower
    // Rethink wins, so the "Rethink outranks Fix" priority within one
    // attempt is best-effort rather than guaranteed. Acceptable —
    // Rethink would surface on the next cycle if the agent's fix didn't
    // address the deeper issue.
    //
    // Dropping `stream` on either short-circuit cancels the still-running
    // reviewer futures; their SSE streams get torn down on drop, so no
    // tokens keep flowing for cancelled principles.
    let mut verdicts = Vec::with_capacity(bundles.len());
    while let Some(result) = stream.next().await {
        match result {
            Ok(v) => {
                let blocking = v.is_blocking();
                verdicts.push(v);
                if blocking {
                    return Ok(verdicts);
                }
            }
            Err(failure) => return Err(failure),
        }
    }
    Ok(verdicts)
}

/// Path of the persisted reviewer audit log under `state_dir`. Public
/// so the engine's `ListReviews` handler reads from the same file the
/// review loop writes to.
pub fn reviews_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("reviews.jsonl")
}

/// Append one entry to `<state_dir>/reviews.jsonl`. JSONL: one
/// `serde_json` object per line. Errors bubble up to the caller, which
/// suppresses the matching `ReviewerCompleted` broadcast on failure so
/// the on-disk log and the live event stream stay in lockstep.
fn append_review_log(state_dir: &Path, entry: &ReviewLogEntry) -> std::io::Result<()> {
    let path = reviews_log_path(state_dir);
    let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .inspect_err(|e| {
            warn!(error = %e, path = %path.display(), "review log: open failed");
        })?;
    f.write_all(line.as_bytes())
        .and_then(|()| f.write_all(b"\n"))
        .inspect_err(|e| {
            warn!(error = %e, path = %path.display(), "review log: write failed");
        })
}

fn to_wire_verdict(k: &VerdictKind) -> ReviewVerdictWire {
    match k {
        VerdictKind::Pass => ReviewVerdictWire::Pass,
        VerdictKind::PassWithNit { reasoning } => ReviewVerdictWire::PassWithNit {
            reasoning: reasoning.clone(),
        },
        VerdictKind::Fail {
            severity,
            reasoning,
            suggested_fix,
        } => ReviewVerdictWire::Fail {
            severity: match severity {
                BlockingSeverity::Fix => ReviewSeverityWire::Fix,
                BlockingSeverity::Rethink => ReviewSeverityWire::Rethink,
            },
            reasoning: reasoning.clone(),
            suggested_fix: suggested_fix.clone(),
        },
    }
}

/// Engine-side truncation of the tool arguments JSON for the sidebar.
/// Keeps wire payloads small (~120 chars) without forcing the chrome to
/// re-format raw JSON. Pure projection — never used to drive any
/// review-loop decision.
fn summarize_args(args: &serde_json::Value) -> String {
    const MAX: usize = 120;
    let raw = serde_json::to_string(args).unwrap_or_default();
    let one_line: String = raw.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one_line.len() <= MAX {
        return one_line;
    }
    let mut s: String = one_line.chars().take(MAX - 1).collect();
    s.push('…');
    s
}

/// Simulate the post-execution file content for `edit` / `write`
/// without touching disk-state. Returns `None` for unsupported tools,
/// missing args, unreadable files, or ambiguous edits — anything the
/// real tool would also reject. Feeding reviewers a misleading
/// "what would have happened" is worse than feeding them none.
fn simulate_artifact(call: &ToolCall) -> Option<String> {
    let path = call.arguments.get("path")?.as_str()?;
    match call.name.as_str() {
        "write" => simulate_write(call),
        "edit" => simulate_edit(call, path),
        "edit_lines" => simulate_edit_lines(call, path),
        _ => None,
    }
}

/// Mirror `lutin_tools::file_edit_lines`: replace the inclusive
/// `start,end` 1-based range with `content`, preserving the file's
/// line-ending style and appending a trailing newline when the
/// replacement otherwise wouldn't terminate. Returns `None` for the
/// same conditions the real tool would reject (missing file, malformed
/// `lines`, out-of-range `start`).
fn simulate_edit_lines(call: &ToolCall, path: &str) -> Option<String> {
    let lines_spec = call.arguments.get("lines")?.as_str()?;
    let content = call.arguments.get("content")?.as_str()?;
    let (start_str, end_str) = lines_spec.trim().split_once(',')?;
    let start: usize = start_str.trim().parse().ok()?;
    let end_signed: i64 = end_str.trim().parse().ok()?;
    if start < 1 || end_signed < start as i64 - 1 {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    let uses_crlf = contents.contains("\r\n");
    let line_ending = if uses_crlf { "\r\n" } else { "\n" };

    let mut line_starts: Vec<usize> = Vec::new();
    if !contents.is_empty() {
        line_starts.push(0);
        for (i, b) in contents.bytes().enumerate() {
            if b == b'\n' && i + 1 < contents.len() {
                line_starts.push(i + 1);
            }
        }
        line_starts.push(contents.len());
    }
    let total_lines = line_starts.len().saturating_sub(1);

    if start > total_lines + 1 {
        return None;
    }
    let end_line: usize = if end_signed < 0 {
        0
    } else {
        (end_signed as usize).min(total_lines)
    };

    let is_insertion = end_line + 1 == start;
    let range_start = if start <= total_lines {
        line_starts[start - 1]
    } else {
        contents.len()
    };
    let range_end = if is_insertion {
        range_start
    } else {
        line_starts[end_line]
    };

    let mut replacement = content.to_string();
    let original_range_ends_with_newline =
        range_end > range_start && contents.as_bytes()[range_end - 1] == b'\n';
    let needs_trailing_newline = !replacement.is_empty()
        && !replacement.ends_with('\n')
        && (original_range_ends_with_newline
            || range_end < contents.len()
            || is_insertion && range_start < contents.len());
    if needs_trailing_newline {
        replacement.push_str(line_ending);
    }
    if uses_crlf {
        let mut out = String::with_capacity(replacement.len() + replacement.matches('\n').count());
        let mut prev: Option<char> = None;
        for c in replacement.chars() {
            if c == '\n' && prev != Some('\r') {
                out.push('\r');
            }
            out.push(c);
            prev = Some(c);
        }
        replacement = out;
    }

    let mut new_contents =
        String::with_capacity(contents.len() - (range_end - range_start) + replacement.len());
    new_contents.push_str(&contents[..range_start]);
    new_contents.push_str(&replacement);
    new_contents.push_str(&contents[range_end..]);
    Some(new_contents)
}

fn simulate_write(call: &ToolCall) -> Option<String> {
    call.arguments
        .get("content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn simulate_edit(call: &ToolCall, path: &str) -> Option<String> {
    let old_string = call.arguments.get("old_string")?.as_str()?;
    if old_string.is_empty() {
        return None;
    }
    let new_string = call.arguments.get("new_string")?.as_str()?;
    let replace_all = call
        .arguments
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let contents = std::fs::read_to_string(path).ok()?;
    if replace_all {
        if !contents.contains(old_string) {
            return None;
        }
        return Some(contents.replace(old_string, new_string));
    }
    let mut hits = contents.match_indices(old_string);
    let first = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    let mut out = String::with_capacity(contents.len() + new_string.len());
    out.push_str(&contents[..first.0]);
    out.push_str(new_string);
    out.push_str(&contents[first.0 + old_string.len()..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lutin_llm::ToolName;
    use serde_json::json;
    use tempfile::tempdir;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: lutin_llm::CallId::new("t"),
            name: ToolName::new(name),
            arguments: args,
        }
    }

    #[test]
    fn simulate_write_returns_content() {
        let c = call("write", json!({"path": "/tmp/x", "content": "hello"}));
        assert_eq!(simulate_artifact(&c).as_deref(), Some("hello"));
    }

    #[test]
    fn simulate_edit_unique_match() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "foo bar baz").unwrap();
        let c = call(
            "edit",
            json!({"path": p.to_str().unwrap(), "old_string": "bar", "new_string": "BAR"}),
        );
        assert_eq!(simulate_artifact(&c).as_deref(), Some("foo BAR baz"));
    }

    #[test]
    fn simulate_edit_ambiguous_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "ab ab").unwrap();
        let c = call(
            "edit",
            json!({"path": p.to_str().unwrap(), "old_string": "ab", "new_string": "X"}),
        );
        assert!(simulate_artifact(&c).is_none());
    }

    #[test]
    fn simulate_edit_replace_all() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "ab ab").unwrap();
        let c = call(
            "edit",
            json!({
                "path": p.to_str().unwrap(),
                "old_string": "ab",
                "new_string": "X",
                "replace_all": true,
            }),
        );
        assert_eq!(simulate_artifact(&c).as_deref(), Some("X X"));
    }

    #[test]
    fn simulate_unknown_tool_is_none() {
        let c = call("bash", json!({"cmd": "ls"}));
        assert!(simulate_artifact(&c).is_none());
    }

    #[test]
    fn simulate_edit_missing_file_is_none() {
        let c = call(
            "edit",
            json!({"path": "/nonexistent/xyz", "old_string": "a", "new_string": "b"}),
        );
        assert!(simulate_artifact(&c).is_none());
    }

    #[test]
    fn simulate_edit_lines_replaces_range() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        let c = call(
            "edit_lines",
            json!({"path": p.to_str().unwrap(), "lines": "2,2", "content": "TWO"}),
        );
        assert_eq!(
            simulate_artifact(&c).as_deref(),
            Some("one\nTWO\nthree\n")
        );
    }

    #[test]
    fn simulate_edit_lines_insertion() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let c = call(
            "edit_lines",
            json!({"path": p.to_str().unwrap(), "lines": "2,1", "content": "MID"}),
        );
        assert_eq!(
            simulate_artifact(&c).as_deref(),
            Some("one\nMID\ntwo\n")
        );
    }

    #[test]
    fn simulate_edit_lines_delete() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        let c = call(
            "edit_lines",
            json!({"path": p.to_str().unwrap(), "lines": "2,2", "content": ""}),
        );
        assert_eq!(simulate_artifact(&c).as_deref(), Some("one\nthree\n"));
    }

    #[test]
    fn simulate_edit_lines_missing_file_is_none() {
        let c = call(
            "edit_lines",
            json!({"path": "/nonexistent/xyz", "lines": "1,1", "content": "x"}),
        );
        assert!(simulate_artifact(&c).is_none());
    }

    #[test]
    fn simulate_edit_lines_bad_range_is_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "one\n").unwrap();
        let c = call(
            "edit_lines",
            json!({"path": p.to_str().unwrap(), "lines": "5,6", "content": "x"}),
        );
        assert!(simulate_artifact(&c).is_none());
    }

    #[test]
    fn is_review_denial_only_matches_tagged() {
        assert!(is_review_denial(
            "denied: <review-deny> rejected by 'X': bad"
        ));
        assert!(!is_review_denial("denied: file not found"));
        assert!(!is_review_denial("file not found"));
    }

    // Behavioral tests through the public `ApprovalPolicy::decide`
    // API. These cover the verdict→approval mapping and the runner
    // request handshake without pinning private helpers.

    use crate::principle::OnMaxRetries;

    fn principle_for(name: &str, applies_to: &[&str]) -> Principle {
        Principle {
            name: name.into(),
            title: name.into(),
            description: "test".into(),
            persona: "test-persona".into(),
            applies_to: applies_to.iter().map(|s| (*s).to_string()).collect(),
            context: vec![ContextItem::ToolCall],
            max_retries: 1,
            on_max_retries: OnMaxRetries::Continue,
        }
    }

    fn bundle_with_response(name: &str, response_json: &str) -> Arc<ReviewerBundle> {
        use lutin_llm::mock::{MockProvider, MockResponse};
        let provider = Arc::new(MockProvider::new(vec![MockResponse::text(response_json)]));
        Arc::new(ReviewerBundle {
            principle: principle_for(name, &["edit"]),
            persona: Persona::default(),
            provider,
            model: ModelId::new("test-model"),
        })
    }

    /// Spawn a runner-task surrogate that drains review requests and
    /// dispatches them to a `ReviewSession`. Returns the join handle
    /// so the test can await it after dropping the policy.
    fn spawn_review_runner(
        mut session: ReviewSession,
        mut req_rx: mpsc::UnboundedReceiver<ReviewRequest>,
    ) -> tokio::task::JoinHandle<ReviewSession> {
        tokio::spawn(async move {
            while let Some(req) = req_rx.recv().await {
                session.handle(req, 0);
            }
            session
        })
    }

    fn edit_call(path: &str) -> ToolCall {
        ToolCall {
            id: lutin_llm::CallId::new("c1"),
            name: ToolName::new("edit"),
            arguments: json!({"path": path, "old_string": "x", "new_string": "y"}),
        }
    }

    /// Drain everything currently buffered on the broadcast subscriber.
    /// Tests must take the subscriber *before* anything is sent — the
    /// receiver only retains messages for active subscribers.
    fn drain(rx: &mut broadcast::Receiver<ChatEvent>) -> Vec<ChatEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Stable label for an event kind, just for sequence assertions.
    fn kind(ev: &ChatEvent) -> &'static str {
        match ev {
            ChatEvent::ReviewFrameOpened { .. } => "Opened",
            ChatEvent::ReviewerStarted { .. } => "Started",
            ChatEvent::ReviewerCompleted { .. } => "Completed",
            ChatEvent::ReviewFrameProgress { .. } => "Progress",
            ChatEvent::ReviewFrameResolved { .. } => "Resolved",
            ChatEvent::AttemptsSquashed { .. } => "Squashed",
            _ => "Other",
        }
    }

    #[tokio::test]
    async fn decide_allows_when_reviewer_passes() {
        let bundle = bundle_with_response("p", r#"{"verdict":"pass","reasoning":"ok"}"#);
        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let (rewind_tx, _rewind_rx) = mpsc::unbounded_channel();
        let (events, _ev_rx) = broadcast::channel(16);
        let mut ev_sub = events.subscribe();
        let dir = tempdir().unwrap();
        let policy = ReviewApproval {
            bundles: vec![bundle.clone()],
            req_tx,
            events: events.clone(),
            next_reviewer_call_id: AtomicU64::new(0),
            state_dir: dir.path().to_path_buf(),
            concurrency: 8,
            rewind_tx: rewind_tx.clone(),
        };
        let session = ReviewSession {
            stack: StepStack::default(),
            bundles: vec![bundle],
            rewind_tx,
            events,
        };
        let runner = spawn_review_runner(session, req_rx);

        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "x").unwrap();
        let approval = policy.decide(&edit_call(p.to_str().unwrap())).await;
        assert!(matches!(approval, Approval::Allow));
        drop(policy);
        let session = runner.await.unwrap();
        assert_eq!(session.stack.frames().len(), 1);
        assert_eq!(session.stack.frames()[0].status, StepStatus::Accepted);

        let evs = drain(&mut ev_sub);
        let seq: Vec<_> = evs.iter().map(kind).collect();
        assert_eq!(seq, vec!["Opened", "Started", "Completed", "Resolved"]);
        assert!(matches!(
            evs.last().unwrap(),
            ChatEvent::ReviewFrameResolved { outcome: ReviewResolution::Accepted, .. }
        ));
    }

    #[tokio::test]
    async fn decide_denies_with_tagged_reason_when_reviewer_fails() {
        let bundle = bundle_with_response(
            "p",
            r#"{"verdict":"fail","severity":"fix","reasoning":"nope"}"#,
        );
        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let (rewind_tx, _rewind_rx) = mpsc::unbounded_channel();
        let (events, _ev_rx) = broadcast::channel(16);
        let mut ev_sub = events.subscribe();
        let dir = tempdir().unwrap();
        let policy = ReviewApproval {
            bundles: vec![bundle.clone()],
            req_tx,
            events: events.clone(),
            next_reviewer_call_id: AtomicU64::new(0),
            state_dir: dir.path().to_path_buf(),
            concurrency: 8,
            rewind_tx: rewind_tx.clone(),
        };
        let session = ReviewSession {
            stack: StepStack::default(),
            bundles: vec![bundle],
            rewind_tx,
            events,
        };
        let runner = spawn_review_runner(session, req_rx);

        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "x").unwrap();
        let approval = policy.decide(&edit_call(p.to_str().unwrap())).await;
        match approval {
            Approval::Deny(reason) => {
                let s = reason.into_owned();
                assert!(s.contains(REVIEW_DENY_TAG), "denial should be tagged: {s}");
                assert!(s.contains("rejected by 'p'"), "should name the principle: {s}");
                assert!(is_review_denial(&format!("denied: {s}")));
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        drop(policy);
        let _ = runner.await;

        let evs = drain(&mut ev_sub);
        let seq: Vec<_> = evs.iter().map(kind).collect();
        assert_eq!(seq, vec!["Opened", "Started", "Completed", "Progress"]);
        match evs.last().unwrap() {
            ChatEvent::ReviewFrameProgress {
                attempt,
                max_attempts,
                blocking,
                ..
            } => {
                assert_eq!(*attempt, 1);
                assert_eq!(*max_attempts, 1);
                assert_eq!(blocking, &vec!["p".to_string()]);
            }
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn decide_rethink_routes_feedback_to_rewind_channel() {
        let bundle = bundle_with_response(
            "p",
            r#"{"verdict":"fail","severity":"rethink","reasoning":"wrong premise"}"#,
        );
        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let (rewind_tx, mut rewind_rx) = mpsc::unbounded_channel();
        let (events, _ev_rx) = broadcast::channel(16);
        let mut ev_sub = events.subscribe();
        let dir = tempdir().unwrap();
        let policy = ReviewApproval {
            bundles: vec![bundle.clone()],
            req_tx,
            events: events.clone(),
            next_reviewer_call_id: AtomicU64::new(0),
            state_dir: dir.path().to_path_buf(),
            concurrency: 8,
            rewind_tx: rewind_tx.clone(),
        };
        let session = ReviewSession {
            stack: StepStack::default(),
            bundles: vec![bundle],
            rewind_tx,
            events,
        };
        let runner = spawn_review_runner(session, req_rx);

        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "x").unwrap();
        let approval = policy.decide(&edit_call(p.to_str().unwrap())).await;
        assert!(matches!(approval, Approval::Deny(_)));
        let signal = rewind_rx.try_recv().expect("rewind signal was queued");
        match signal {
            RewindSignal::Continue { feedback } => {
                assert!(feedback.contains("wrong premise"));
            }
            other => panic!("expected RewindSignal::Continue, got {other:?}"),
        }
        drop(policy);
        let session = runner.await.unwrap();
        assert_eq!(session.stack.frames()[0].status, StepStatus::Abandoned);

        let evs = drain(&mut ev_sub);
        let seq: Vec<_> = evs.iter().map(kind).collect();
        // `Squashed` precedes `Resolved` on a Rethink: the Rewound
        // attempt is squashed live so the UI can drop its bubble while
        // the iteration-box outline (which goes away on Resolved) is
        // still anchored.
        assert_eq!(
            seq,
            vec!["Opened", "Started", "Completed", "Squashed", "Resolved"]
        );
        match evs.last().unwrap() {
            ChatEvent::ReviewFrameResolved {
                outcome: ReviewResolution::Rewound { feedback },
                ..
            } => assert!(feedback.contains("wrong premise")),
            other => panic!("expected Resolved::Rewound, got {other:?}"),
        }
    }
}
