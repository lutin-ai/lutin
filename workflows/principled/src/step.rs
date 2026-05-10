//! Step frame stack + snapshot rewind primitives.
//!
//! Each reviewable tool call becomes a `StepFrame` pushed onto the
//! session's stack. A `Snapshot` is captured before the tool runs so
//! a downstream rewind can restore the prior state — file contents
//! the tool would mutate, plus the conversation index so any
//! tool-result entries appended after the snapshot can be truncated.
//!
//! This module owns *only* the data types and the
//! capture/restore mechanics. The review loop that drives
//! reviewers, decides retry-vs-rewind, and pushes/pops frames lives
//! in the next layer up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::principle::OnMaxRetries;

/// Monotonic step identifier. Local to a session — not persisted
/// across reloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    /// Active in the review loop: reviewers running or adjustments in
    /// flight. The tool may or may not have executed yet — the loop
    /// owns that decision.
    Active,
    /// Reviewers all passed/skipped; tool ran; entry recorded in
    /// the transcript. Visible in the chat.
    Accepted,
    /// Either rewound from above (popped) or replaced by a fresh
    /// attempt. Lives on in the sidebar audit log only.
    Abandoned,
}

/// Per-reviewer per-step state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerSlot {
    /// Number of *failure* iterations this principle has consumed for
    /// the current step. Sticky-passing: a passing iteration does not
    /// increment, but a later failure of the same principle still
    /// draws from this same budget.
    pub attempts: u32,
    pub status: ReviewerSlotStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewerSlotStatus {
    /// Not yet evaluated this iteration, or last verdict was a fail
    /// that hasn't yet been resolved.
    Pending,
    /// Last verdict was pass (or pass-with-nit). Loop won't try to fix
    /// this principle again unless it fails in a later iteration.
    Passing,
    /// Hit `max_retries`; loop won't run this reviewer again for the
    /// rest of this step. The `on_max_retries` resolution at the
    /// moment of skipping is recorded so the UI can explain why.
    Skipped { resolution: OnMaxRetries },
}

/// Severity that *blocks* execution. Pass-with-note ("nit") doesn't
/// block, so it's not part of this enum — `Verdict::PassWithNit` is
/// expressed in the variant itself, not via an `Option<Severity>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockingSeverity {
    /// Main agent must adjust this step.
    Fix,
    /// Loop pops a frame and rewinds further back.
    Rethink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub principle_name: String,
    pub kind: VerdictKind,
}

/// Verdict outcome. The variants encode every legal combination of
/// pass-or-fail × severity-or-not — a `pass + Severity::Fix` cannot
/// be constructed, so the comment that used to say "severity present
/// only when failed *or* nit" no longer needs to exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictKind {
    /// Reviewer passed cleanly. No reasoning surfaced.
    Pass,
    /// Reviewer passed but left a note. Reasoning is shown in the
    /// sidebar but does not block execution.
    PassWithNit { reasoning: String },
    /// Reviewer rejected. The severity decides whether the loop
    /// adjusts in place (Fix) or rewinds (Rethink).
    Fail {
        severity: BlockingSeverity,
        reasoning: String,
        suggested_fix: Option<String>,
    },
}

impl Verdict {
    pub fn is_blocking(&self) -> bool {
        matches!(self.kind, VerdictKind::Fail { .. })
    }
    pub fn reasoning(&self) -> Option<&str> {
        match &self.kind {
            VerdictKind::Pass => None,
            VerdictKind::PassWithNit { reasoning } => Some(reasoning.as_str()),
            VerdictKind::Fail { reasoning, .. } => Some(reasoning.as_str()),
        }
    }
}

/// What happened to one attempt at this step. Replaces the older
/// `executed: bool` so call sites can't accidentally mark an attempt
/// as both executed *and* abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptOutcome {
    /// Reviewers approved; tool ran.
    Executed,
    /// Blocked by a Fix verdict; the agent retried.
    DeniedRetry,
    /// A principle exceeded `max_retries` with `on_max_retries =
    /// AskUser`; the turn paused for the user.
    Escalated,
    /// A Rethink verdict popped this frame off the stack.
    Rewound,
}

/// One try at this step's tool call. A step accumulates attempts
/// during the review loop; the final accepted attempt is the one
/// whose tool actually ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    /// Tool-call id from the agent SDK; matches the `call_id` on the
    /// emitted `ToolCallStreaming` / `ToolCallCompleted` events. The
    /// engine uses this to tell the UI which streamed tool bubbles
    /// belong to a step that has now resolved (so denied attempts can
    /// disappear live, before end-of-turn squash).
    pub call_id: String,
    /// Tool name + args at the moment of the attempt. JSON-encoded
    /// args to stay protocol-shape-agnostic.
    pub tool_name: String,
    pub arguments_json: String,
    /// Verdicts collected for this attempt. Order matches principle
    /// order (least-important first).
    pub verdicts: Vec<Verdict>,
    pub outcome: AttemptOutcome,
}

/// Captured pre-execution state for a single step. Restored verbatim
/// when the frame is rewound or abandoned.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// File paths the step's tool would mutate, mapped to their
    /// contents at snapshot time. `None` means the file did not
    /// exist (restore = delete it).
    pub files: HashMap<PathBuf, Option<Vec<u8>>>,
    /// Number of transcript entries before the step's tool ran. On
    /// restore, truncate the transcript back to this length.
    ///
    /// **Invariant**: while any frame is `Active`, the transcript must
    /// be append-only — `EditMessage`/`DeleteMessage`/`DeleteFromHere`
    /// would shift the index and silently corrupt rewinds. The review
    /// loop driver enforces this at the request handler level by
    /// rejecting mutations whenever the stack has an active frame.
    pub conversation_index: usize,
}

impl Snapshot {
    /// Snapshot one file's current content (or absence). Idempotent:
    /// re-snapshotting a path overwrites — the *first* call within a
    /// step is the one that matters, but the loop is responsible for
    /// not double-snapshotting.
    pub fn capture_file(&mut self, path: &Path) -> std::io::Result<()> {
        match std::fs::read(path) {
            Ok(bytes) => {
                self.files.insert(path.to_path_buf(), Some(bytes));
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.files.insert(path.to_path_buf(), None);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Restore every captured file. Returns the first error encountered
    /// but attempts the rest — partial restoration is preferable to
    /// stopping at the first failure when rolling back a bad state.
    pub fn restore_files(&self) -> std::io::Result<()> {
        let mut first_err: Option<std::io::Error> = None;
        for (path, prior) in &self.files {
            let res = match prior {
                Some(bytes) => std::fs::write(path, bytes),
                None => match std::fs::remove_file(path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    other => other,
                },
            };
            if let Err(e) = res {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[derive(Debug)]
pub struct StepFrame {
    pub id: StepId,
    pub status: StepStatus,
    pub attempts: Vec<AttemptRecord>,
    /// One slot per principle, in the order the principles were
    /// declared (least-important-first). Order matters: the review
    /// loop adjusts the lowest-priority failing principle first, so a
    /// stable, ordered structure is intrinsic to the design.
    pub reviewers: Vec<(String, ReviewerSlot)>,
    pub snapshot: Snapshot,
    /// Feedback inherited from a future frame that was rewound back
    /// through this one. Concatenated each time a downstream
    /// `Rethink` lands here. Fed to the main agent on the next
    /// adjustment so it knows what went wrong further along.
    pub carried_forward: String,
    /// Set when this frame was created by popping a higher frame.
    /// Pure provenance — not used in the current control flow but
    /// kept for sidebar audit rendering.
    pub rewound_from: Option<StepId>,
    /// Tool name the agent committed to on this step's first attempt.
    /// Subsequent retries within the same `Active` frame are gated to
    /// this name (or `abort_step`) by the review approval policy —
    /// keeps the agent from drifting onto unrelated tools mid-step,
    /// which is otherwise possible after compaction summarizes away
    /// the iteration context. Set at frame construction (a frame
    /// always has a triggering tool call), never cleared — survives
    /// the Active → Accepted transition so audit views and any future
    /// reactivation via rewind still see the original lock.
    pub iterated_tool: String,
}

impl StepFrame {
    /// `iterated_tool` is the tool name from the call that triggered
    /// this frame — every frame has one (a frame is created in response
    /// to a tool call going through approval). Embedding it in the
    /// constructor instead of leaving it `Option` makes "Active frame
    /// without a lock" unrepresentable.
    pub fn new(
        id: StepId,
        conversation_index: usize,
        principle_names: &[&str],
        iterated_tool: String,
    ) -> Self {
        let reviewers = principle_names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ReviewerSlot {
                        attempts: 0,
                        status: ReviewerSlotStatus::Pending,
                    },
                )
            })
            .collect();
        Self {
            id,
            status: StepStatus::Active,
            attempts: Vec::new(),
            reviewers,
            snapshot: Snapshot {
                files: HashMap::new(),
                conversation_index,
            },
            carried_forward: String::new(),
            rewound_from: None,
            iterated_tool,
        }
    }
}

/// Bottom-up stack of step frames. The last entry is the active
/// frame; earlier entries are accepted (or, transiently during
/// rewind, being re-activated).
#[derive(Debug, Default)]
pub struct StepStack {
    frames: Vec<StepFrame>,
    next_id: u64,
}

/// Outcome of attempting to rewind one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindOutcome {
    /// Popped the top frame and re-activated the prior one with
    /// `carried_forward` extended. The caller should re-enter the
    /// review loop for the now-active frame.
    Rewound { reactivated: StepId },
    /// The failed step was at the bottom of the stack — nothing to
    /// pop to. Caller should surface to the user.
    BottomOfStack,
}

impl StepStack {
    pub fn next_id(&mut self) -> StepId {
        let id = StepId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn push(&mut self, frame: StepFrame) {
        self.frames.push(frame);
    }

    pub fn active(&self) -> Option<&StepFrame> {
        self.frames.last()
    }

    pub fn frames(&self) -> &[StepFrame] {
        &self.frames
    }

    pub fn frames_mut(&mut self) -> &mut Vec<StepFrame> {
        &mut self.frames
    }

    /// Mark the current active frame accepted. The caller is expected
    /// to have run the tool and recorded the executed attempt before
    /// calling this.
    #[allow(dead_code)] // used by tests; production callers land with the sidebar slice
    pub fn accept_active(&mut self) {
        if let Some(f) = self.frames.last_mut() {
            f.status = StepStatus::Accepted;
        }
    }

    /// Pop the active frame, restore its snapshot, and re-activate the
    /// frame below with the rewound feedback appended. Returns
    /// `BottomOfStack` if the active frame is the only one.
    pub fn rewind(&mut self, feedback: &str) -> std::io::Result<RewindOutcome> {
        let Some(mut top) = self.frames.pop() else {
            return Ok(RewindOutcome::BottomOfStack);
        };
        // Restore top's pre-execution file state. Conversation
        // truncation lives with the caller (it owns the transcript).
        top.snapshot.restore_files()?;
        top.status = StepStatus::Abandoned;

        let Some(prior) = self.frames.last_mut() else {
            // Top was the only frame; re-push it (abandoned) so the
            // sidebar can still see the trail, then signal the
            // caller. Returning ownership of the abandoned frame
            // would be cleaner but the sidebar reads through
            // `frames()`.
            self.frames.push(top);
            return Ok(RewindOutcome::BottomOfStack);
        };

        // Do NOT restore the prior frame's snapshot — that frame was
        // already accepted by reviewers, and rolling back its edits
        // makes the agent see its own approved work vanish.
        prior.status = StepStatus::Active;
        if !prior.carried_forward.is_empty() {
            prior.carried_forward.push_str("\n\n");
        }
        prior.carried_forward.push_str(feedback);
        let rewound_from = top.id;
        prior.rewound_from = Some(rewound_from);

        let reactivated = prior.id;

        // Keep the abandoned top in the stack? No — sidebar reads
        // attempts/verdicts from a separate audit log, not from the
        // stack. The stack is only the path of accepted+active
        // frames. Drop `top`.
        drop(top);

        Ok(RewindOutcome::Rewound { reactivated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_captures_existing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"before").unwrap();

        let mut snap = Snapshot::default();
        snap.capture_file(&p).unwrap();

        std::fs::write(&p, b"after").unwrap();
        snap.restore_files().unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), b"before");
    }

    #[test]
    fn snapshot_captures_missing_file_and_deletes_on_restore() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.txt");

        let mut snap = Snapshot::default();
        snap.capture_file(&p).unwrap();

        std::fs::write(&p, b"created later").unwrap();
        snap.restore_files().unwrap();

        assert!(!p.exists(), "file should have been deleted on restore");
    }

    #[test]
    fn restore_is_idempotent_when_nothing_changed() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"x").unwrap();
        let mut snap = Snapshot::default();
        snap.capture_file(&p).unwrap();
        snap.restore_files().unwrap();
        snap.restore_files().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"x");
    }

    #[test]
    fn step_frame_initializes_reviewer_slots() {
        let f = StepFrame::new(StepId(0), 7, &["a", "b"], "edit".into());
        assert_eq!(f.snapshot.conversation_index, 7);
        assert_eq!(f.reviewers.len(), 2);
        for (_name, slot) in &f.reviewers {
            assert_eq!(slot.attempts, 0);
            assert_eq!(slot.status, ReviewerSlotStatus::Pending);
        }
    }

    #[test]
    fn rewind_pops_top_and_reactivates_prior() {
        let mut stack = StepStack::default();
        let id0 = stack.next_id();
        stack.push(StepFrame::new(id0, 0, &["p"], "edit".into()));
        stack.accept_active();
        let id1 = stack.next_id();
        stack.push(StepFrame::new(id1, 5, &["p"], "edit".into()));

        let out = stack.rewind("future said: this approach was wrong").unwrap();
        assert_eq!(out, RewindOutcome::Rewound { reactivated: id0 });

        let active = stack.active().expect("prior frame should be active");
        assert_eq!(active.id, id0);
        assert_eq!(active.status, StepStatus::Active);
        assert_eq!(active.rewound_from, Some(id1));
        assert!(active.carried_forward.contains("approach was wrong"));
        assert_eq!(stack.frames().len(), 1, "rewound frame is dropped");
    }

    #[test]
    fn rewind_at_bottom_signals_escalation() {
        let mut stack = StepStack::default();
        let id0 = stack.next_id();
        stack.push(StepFrame::new(id0, 0, &["p"], "edit".into()));

        let out = stack.rewind("nothing below").unwrap();
        assert_eq!(out, RewindOutcome::BottomOfStack);
        // The frame is still present (now abandoned) for audit.
        assert_eq!(stack.frames().len(), 1);
        assert_eq!(stack.active().unwrap().status, StepStatus::Abandoned);
    }

    #[test]
    fn rewind_accumulates_carried_forward() {
        let mut stack = StepStack::default();
        let id0 = stack.next_id();
        stack.push(StepFrame::new(id0, 0, &["p"], "edit".into()));
        stack.accept_active();
        let id1 = stack.next_id();
        stack.push(StepFrame::new(id1, 1, &["p"], "edit".into()));
        stack.rewind("first rewind").unwrap();

        // After rewinding, push a new attempt frame and rewind again.
        let id2 = stack.next_id();
        stack.push(StepFrame::new(id2, 1, &["p"], "edit".into()));
        stack.rewind("second rewind").unwrap();

        let active = stack.active().unwrap();
        assert!(active.carried_forward.contains("first rewind"));
        assert!(active.carried_forward.contains("second rewind"));
    }

    #[test]
    fn rewind_restores_only_top_frame_files() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"a0").unwrap();
        std::fs::write(&b, b"b0").unwrap();

        let mut stack = StepStack::default();
        let id0 = stack.next_id();
        let mut f0 = StepFrame::new(id0, 0, &[], "edit".into());
        f0.snapshot.capture_file(&a).unwrap();
        stack.push(f0);
        std::fs::write(&a, b"a1").unwrap();
        stack.accept_active();

        let id1 = stack.next_id();
        let mut f1 = StepFrame::new(id1, 0, &[], "edit".into());
        f1.snapshot.capture_file(&b).unwrap();
        stack.push(f1);
        std::fs::write(&b, b"b1").unwrap();

        stack.rewind("rewind").unwrap();

        // Only the rewound (top) frame's file reverts. The prior
        // frame was already accepted by reviewers, so its edits stay.
        assert_eq!(std::fs::read(&a).unwrap(), b"a1");
        assert_eq!(std::fs::read(&b).unwrap(), b"b0");
    }
}
