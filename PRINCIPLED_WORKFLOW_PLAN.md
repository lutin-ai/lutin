# Principled workflow plan

A new workflow that behaves like the chat workflow but gates every tool call behind a configurable set of single-principle reviewer agents. Reviewers can force a retry or rewind to a prior step. The mechanism is generic over tool kind and domain — instantiate it as a coder workflow by picking coding principles, as a researcher workflow by picking research principles, etc.

Workflow name: `principled` (committed). Forked from `workflows/chat/` → `workflows/principled/`; protocol package forked from `packages/chat-protocol/` → `packages/principled-protocol/`.

## Inspiration

This is Tree-of-Thoughts in shape, simplified to a linear stack: each tool call is a step; reviewers gate the step; on failure the loop adjusts in place or pops a frame and carries the future feedback forward.

## Concepts

### Step
One tool call from the main agent = one step. If the agent emits multiple tool calls in a single turn, only the **first** is taken; the rest are discarded. (The first call's outcome will likely affect what comes next anyway.)

### Principle
A single rule, evaluated by one reviewer. Configured in TOML:

```toml
# personas/principles/guard-clauses.toml
[principle]
title = "Guard clauses over nesting"
description = """
Long-form text the reviewer reads. Explains the rule, examples,
edge cases. The reviewer's whole job is judging this one principle.
"""
persona = "reviewer-rust"           # picks model + system prompt frame
applies_to = ["edit", "write"]      # which tool names trigger this principle
context = ["tool_call", "tool_artifact", "file"]
                                    # which info the reviewer sees
max_retries = 3                     # per-principle, per-step
on_max_retries = "continue"         # or "ask_user"
```

`context` vocabulary (tool-agnostic):

- `tool_call` — implicit, always included. Tool name + args.
- `tool_artifact` — for tools where it can be computed without execution. Currently only Edit/Write (post-edit file content). Others omit this.
- `chat` — the main user-agent conversation so far.
- `prior_steps` — accepted prior frames (their tool calls + results).

### Reviewer
A stateless one-shot LLM call driven by a principle. Outputs structured JSON:

```ts
{
  verdict: "pass" | "fail",
  severity?: "nit" | "fix" | "rethink",  // present only on fail
  reasoning: string,
  suggested_fix?: string                 // optional, principle-specific
}
```

- `nit` — pass with a note; does not block.
- `fix` — block; main agent must retry this step.
- `rethink` — block; loop pops a frame and rewinds.

### Step frame (rewindable unit)
Implemented in `workflows/principled/src/step.rs`:
```rust
StepFrame {
  id: StepId
  status: Active | Accepted | Abandoned
  attempts: Vec<AttemptRecord>
  // Vec, not HashMap — ordering is intrinsic (least-important-first
  // matches the principle declaration order; the loop iterates in
  // priority order when picking which failure to address next).
  reviewers: Vec<(String, ReviewerSlot { attempts, status })>
  // status: Pending | Passing | Skipped { resolution: OnMaxRetries }
  rewound_from: Option<StepId>
  carried_forward: String
  snapshot: Snapshot {
    files: HashMap<PathBuf, Option<Vec<u8>>>  // None = file didn't exist
    conversation_index: usize                 // truncate transcript on restore
  }
}
```

**Conversation snapshot is index-based**, not a full clone. Invariant: while any frame is `Active`, the transcript must stay append-only. The review loop driver enforces this by rejecting `EditMessage`/`DeleteMessage`/`DeleteFromHere` requests whenever the stack has an active frame (a new `ChatError` variant — added when wiring the stack into `AppState`).

The session is a stack of frames. The visible chat = `accepted` frames in order. `abandoned` frames stay in the sidebar log only.

The `snapshot` is taken **before** the step's tool executes and is the rewind primitive. It covers both file mutations (revert disk) and conversation state (drop tool results the agent saw, e.g. Grep output). Same primitive for mutating and non-mutating tools.

## Review loop (per step)

All review is **pre-exec** — the tool does not run until reviewers approve. For Edit/Write, the workflow simulates the result (applies the edit in memory) and shows reviewers the post-edit file as `tool_artifact`. Other tools have no artifact pre-exec; reviewers judge intent based on tool call + chat + prior steps.

```
1. Main agent emits a tool_use. Discard all but the first.
2. Find principles where step.toolCall.name ∈ principle.applies_to.
   If none → execute tool immediately, no sidebar entry, no frame review state.
3. Compute artifact if applicable (Edit/Write → simulate post-edit file).
4. Run all matching reviewers in parallel. Each reviewer is a one-shot
   call with persona + principle + selected context.
5. Collect verdicts. Separate fails from passes/nits.
6. If no fails (or only nits) → execute tool, mark step accepted, move on.
7. If any fail with severity = "rethink" → REWIND (see Rewind mechanics).
   "rethink" outranks "fix": if both severities are present, rewind wins.
8. Else (only "fix" failures) → pick the LEAST-important failed principle
   (first in the principle ordering). Increment its attempts.
   - If its attempts > max_retries: mark this principle "skipped" for this
     step. Honor on_max_retries:
       - "continue" → just skip, loop continues with remaining failures
       - "ask_user" → pause, surface to user, await direction
   - Else: send the main agent a tool_result like
     "rejected by 'Guard clauses': <reasoning>. Please adjust." Main agent
     emits a new tool_use. Goto 1 (with a fresh review pass that re-runs
     ALL reviewers, not just the one that failed).
9. Loop ends when every reviewer is `passing` or `skipped`.
```

Any new tool call from the main agent while a review loop is active is treated as an **adjustment** to the current step — the previous attempt is replaced regardless of which tool/path the agent picks. The agent is not supposed to start a new step until the current one is accepted. If it tries, the workflow forces the call into the current loop.

### Why least-important first
Adjustments later in the queue may overwrite earlier ones. Doing the most-important fix last means the most-important principle "gets the last word." Oscillation (A↔B) is theoretically possible but bounded by per-principle `max_retries`; in practice prompt discipline ("change only what this principle requires; do not modify anything else") suppresses it. If oscillation does deadlock, `on_max_retries = "ask_user"` is the escape valve.

### Why re-run all reviewers each iteration
Fixing principle B can break principle A. Re-running all reviewers converges to a real fixed point. With cheap reviewer personas (Haiku-class) the cost is acceptable.

### Reviewer attempt counter semantics
`attempts` only increments on **failure**. Passing is sticky for the current iteration but not permanent — a principle that previously passed can fail in a later iteration of the same step's loop, drawing from the same retry budget. A principle in `skipped` state stays skipped for the rest of the step.

### Cost note
Worst case is `(simultaneous failures) × (full reviewer fan-out)` LLM calls per step. Five principles failing simultaneously, resolved one at a time, means five rounds of full re-review. With Haiku-class reviewer personas this is tolerable for v1. Future optimization: cache verdicts keyed on (principle, input hash) and skip re-running stable passers.

## Rewind mechanics

Rewinding from frame N (the failed step) to frame N−1:

1. Mark frame N as `abandoned`. Restore its snapshot (files + conversation state) — this undoes nothing for N itself since N didn't execute, but ensures we're at the state just before N was attempted.
2. Pop frame N off the stack.
3. Frame N−1 was previously `accepted`. To revert it:
   - Restore N−1's snapshot (revert N−1's file mutations and remove its tool result from the main agent's conversation).
   - Set N−1's status back to `active`.
   - Append the rewind feedback to N−1's `carriedForward` field.
4. Restart N−1's review loop. The main agent is given the carried-forward feedback as a hint ("a prior attempt at the next step failed because: <feedback>. Choose differently.") and emits a new tool call for the N−1 slot.

### Bottom-of-stack rewind
If the failed step is the first step in the session (nothing to pop to), the workflow surfaces the rethink feedback to the user and pauses, with the same UX as `on_max_retries = "ask_user"`. Don't loop forever or silently accept.

### Cascading rewinds
If after rewinding to N−1, N−1's reviewers also produce a `rethink`, the same rewind procedure applies — pop to N−2, accumulate carried-forward feedback. This is how "go back further, further, further" works.

## Main-agent context shaping

During the review loop, all attempts stay visible to the main agent — it needs to see the prior baseline to make targeted fixes:

```
[assistant] tool_use: Edit(foo.rs, v1)
[tool_result] rejected by "Guard clauses": <reasoning>. Please adjust.
[assistant] tool_use: Edit(foo.rs, v2)
[tool_result] rejected by "Use Vec": <reasoning>. Please adjust.
[assistant] tool_use: Edit(foo.rs, v3)
[tool_result] applied.
```

**On final acceptance**, squash the run of attempts in the main agent's transcript to a single `tool_use → tool_result(applied)`. Sidebar keeps everything. This bounds context bloat per step.

## Tools with unrecoverable side effects

Network POSTs, real Bash that touched the world, paid sub-agent calls — once executed, the world's state can't be reverted. Since all review is pre-exec, the *world* never sees rejected attempts. The remaining hazard is approved-then-rewound: a tool was approved, executed, and later a `rethink` from a downstream step pops back through it. The conversation snapshot redacts the result from the agent's view, but the side effect happened.

For v1: accept this. Document it. If it becomes a real problem, add a per-principle or per-tool flag like `rewindable = false` that prevents downstream rethinks from popping past such a step (instead surfacing to the user).

## UI

Two-pane layout, mirroring the chat workflow but with a reviewer log sidebar.

### Main chat
Looks like a normal chat. Renders only `accepted` frames. The main agent's text response (narration before/around tool use) streams to the chat as soon as it arrives — only the *tool call rendering* is gated until the step is accepted.

While a review loop is active, show an inline indicator in place of the pending tool call:

```
reviewing edit foo.rs · attempt 2/3 · 1 principle blocking ▾
```

Click to expand for the live reviewer state (which principles passed, which are blocking, current reasoning). On acceptance, the indicator collapses into a normal tool-call rendering.

If a step gets rewound, its in-progress chat indicator simply disappears — no abandoned UI in the chat itself. The audit trail is the sidebar.

### Reviewer sidebar
A log of every reviewer invocation, in wall-clock order:

```
[hh:mm:ss] reviewer-persona · principle-title · tool-name(short args)
  verdict: pass / fail (severity)
  reasoning (collapsed by default, click to expand)
  [optional: suggested_fix]
```

Filterable by reviewer persona, by principle, by verdict, by step. Clicking an entry highlights the corresponding step in the chat if that step was accepted; otherwise shows "abandoned (rewound)."

The sidebar is the audit/debug view — it sees abandoned attempts that the chat doesn't. Persisted alongside the chat history (survives reload).

## File layout

```
workflows/principled/                 # Rust engine + UI (forked from chat)
  src/
    engine.rs    # bin entry — needs review-loop wiring
    lib.rs       # protocol types (ChatRequest/Event/Ok/Error)
    principle.rs # ★ new: Principle DTO + load/list (mirrors Persona)
    step.rs      # ★ new: StepFrame, StepStack, Snapshot, Verdict, ...
    agents.rs, store.rs, tools/  # carried over from chat
  ui/src/
    App.tsx, adapter.ts, session.ts, lutin.ts, ...   # carried over
    # to add: reviewer.ts (parallel fan-out), ReviewerSidebar.tsx
packages/principled-protocol/         # ★ forked from chat-protocol
  src/{index,chat,postcard}.ts
```

Principles live at `<config_dir>/principles/<name>.toml` — top-level under each scope (global + project), discovered via `lutin_storage::Resolver` exactly like `personas/`. Project tier wins on name clash. (Earlier draft said `personas/principles/`; final is top-level `principles/`.)

## Per-session configuration

Principle list and persona are **per-session**, stored in `SessionState` (persisted to `<state_dir>/state.toml`), not in the workflow bundle manifest. Different sessions of the same workflow can have different review setups:

```rust
SessionState {
  persona: Option<String>,          // existing — same as chat
  model_override: Option<String>,
  principles: Vec<String>,          // ★ new: principle stems, least-important-first
}
```

Defaults: `principles = []` for a new session, which makes principled behave exactly like chat (no review gating). User opts in via the picker UI (task #7), or by editing `state.toml` directly for now.

Discovery: a future `ListPrinciples` request will scan via `Resolver` (parallels `ListPersonas`). Deferred to task #7 alongside the picker UI.

`lutin.workflow.json` is the chrome-facing bundle manifest — display name, icon, capabilities — and is **not** the place for engine config.

## Example principles across domains

Coding:
- `prefer-vec-over-hashmap` — applies to `["edit", "write"]`
- `no-deep-nesting` / `guard-clauses` — applies to `["edit", "write"]`
- `edit-size-limit` — applies to `["edit", "write"]`

Research:
- `cite-primary-sources` — applies to `["web_search", "web_fetch"]`
- `no-redundant-searches` — applies to `["web_search"]`

Planning / orchestration:
- `subagent-prompt-quality` — applies to `["spawn_agent"]`
- `bash-no-destructive` — applies to `["bash"]`

## Open questions deferred to implementation

(Resolved: name committed as `principled`. Protocol forked rather than extended. Chat infrastructure forked rather than extracted — extract later if it earns it. The "1 tool call per turn" cap is enforced in the workflow via `tool_policy.max_calls_per_round = 1` whenever the session has principles configured — see task 6 below.)

## Scope for v1

- [x] **Fork** `workflows/chat/` → `workflows/principled/` (binary `principled`, manifest, UI bundle, identity strings, `use chat::` → `use principled::`). Protocol forked to `packages/principled-protocol/`. Rust compiles; `bun install` at repo root needed before UI builds.
- [x] **Per-session principles config**: `SessionState.principles: Vec<String>` with `#[serde(default)]`. Wire-format extended: `Subscribed{empty}` golden bytes are 6 bytes now (extra `0x00` for empty Vec). Both Rust + TS golden tests updated.
- [x] **Principle TOML loader** at `<config>/principles/<name>.toml`, project-over-global via `Resolver`. `workflows/principled/src/principle.rs`. Schema: `title, description, persona, applies_to, context, max_retries, on_max_retries`. `ContextItem`: `ToolCall | ToolArtifact | Chat | PriorSteps` (strict, unknown values fail to parse). `OnMaxRetries`: `Continue (default) | AskUser`. 5 unit tests.
- [x] **Step frame + snapshot rewind** in `workflows/principled/src/step.rs`. `StepFrame`, `StepStack`, `Snapshot { files, conversation_index }`, `Verdict`, `AttemptRecord`, `RewindOutcome` (incl. `BottomOfStack`). Reviewers ordered Vec, not HashMap. Snapshot covers file content (incl. missing→delete) + conversation index. `rewind` pops top, restores both top+prior file snapshots, re-activates prior with appended `carried_forward`. 8 unit tests.
- [x] **Review loop driver** — *foundation + rewind + pre-exec artifact + apply guard landed*: `reviewer.rs` (one-shot reviewer LLM call + verdict parser), `review.rs` (`ApprovalPolicy` impl: matches principles by `applies_to`, fans out reviewers in parallel, picks least-important failure, honours per-principle `max_retries` + `on_max_retries`, captures file snapshot for `edit`/`write` tool kinds at frame push). Wired into `run_turn` after `sdk_refresh_agent` (per-turn so on-disk edits take effect). The SDK's per-round loop drives "agent retries with a new tool call" naturally on `Deny`; rethink verdicts queue a rewind via an `mpsc::UnboundedSender<()>`, the runner observes it through an extra branch in the `tokio::select!`, cancels the agent, and `perform_rewind` does: pop frame, restore both file snapshots, truncate `agent.messages()` + `entries` to the prior frame's `conversation_index`, append a synthetic User message carrying the carried_forward feedback, and `agent.start()` again. `BottomOfStack` ends the turn with `FinishReason::Failed` carrying the escalation reason. Live transcript-length tracking (`update_live_messages_len` on `AssistantMessage` / `ToolCallCompleted` events) keeps `ReviewState.live_messages_len` accurate so each new frame's snapshot index matches the row a future rewind would truncate to. `ChatError::ReviewInFlight` added (Rust + TS protocol package). **Pre-exec artifact**: `simulate_artifact()` reads the on-disk file for `edit` (find/replace, ambiguity = `None`) and returns `content` directly for `write`; only computed when at least one matching reviewer opted into `ContextItem::ToolArtifact`, and only forwarded to those reviewers. **Session-scoped review state**: `ReviewState` promoted onto `RunnerCtx` so the step stack persists across turns — sets up future cross-turn rewind and lets `apply_mutation` reject transcript edits whenever any frame is `Active` (returns `ChatError::ReviewInFlight`). The originally-deferred items (conversation squashing on accept, multi-tool-call cap) all landed in task 6 below. 6 new artifact-simulation tests added; 43 unit tests pass overall at the end of this slice.
- [x] **Main-agent context shaping** — `squash_denied_attempts()` walks the agent transcript at end-of-turn (post-`join`, pre-`sync_new_entries`) and removes `ToolResult{is_error, content="denied: <review-deny> …"}` rows along with the matching tool_call from the preceding `Assistant` (drops the whole assistant message if its `tool_calls` becomes empty). Detection routes through `review::is_review_denial` so genuine in-tool errors aren't squashed. Multi-tool cap: `agent.update_config` sets `tool_policy.max_calls_per_round = 1` whenever the session has principles configured. Adjustment-during-active-loop was already implicit: the runner-side `ReviewSession::begin_frame` reuses the active frame on retries rather than pushing a new one.

- [x] **Principles-driven review hardening** (post-review fixes from running 29 software-principle reviewers over the code):
  - **Channel-owned `ReviewState`**: `Arc<Mutex<ReviewState>>` deleted. The runner task now owns the step stack as a `ReviewSession`; `ApprovalPolicy::decide` talks to it over `mpsc<ReviewRequest>` (`BeginFrame` + `ApplyVerdicts`) and awaits replies on per-call oneshots. Single writer, no shared mutex, no `unwrap()` on `lock()`.
  - **`live_messages_len` derived from one writer**: lives in `run_turn`'s scope, updated only by `update_live_messages_len(&mut usize, &AgentEvent)`, forwarded into `ReviewSession::begin_frame` per request.
  - **`Verdict` enum collapse**: `Verdict { passed: bool, severity: Option<Severity>, ... }` → `Verdict { principle_name, kind: VerdictKind { Pass | PassWithNit{reasoning} | Fail{ severity: BlockingSeverity, reasoning, suggested_fix } } }`. `pass + Severity::Fix` no longer representable. `Severity::Nit` removed (encoded by `PassWithNit` variant).
  - **`AttemptOutcome` enum**: `executed: bool` → `outcome: AttemptOutcome { Executed | DeniedRetry | Escalated | Rewound }`. Each call site picks one at construction; the inconsistent "Accepted but not executed" combo is gone.
  - **Typed denial sentinel**: every review-loop `Approval::Deny` now goes through `deny_reason()` which prepends `<review-deny>`. `is_review_denial()` predicate (used by `squash_denied_attempts` and a new test) reads the tag — no more stringly-typed `"denied:"` prefix matching.
  - **`build()` shape**: `Result<Option<ReviewApproval>, _>` → `Result<(ReviewApproval, ReviewSession), _>`. Caller short-circuits on empty principle list; `debug_assert!` enforces the contract.
  - **Error propagation**: policy install failures (`build`, `try_set_approval`, `update_config(max_calls)`) end the turn with `FinishReason::Failed` instead of warning and running unreviewed.
  - **Behavioral tests through `decide`**: 3 new `#[tokio::test]` cases drive `ReviewApproval::decide` against `MockProvider` reviewers and a runner-task surrogate that processes `ReviewRequest`s — they verify Allow/Deny/Rethink mappings and the rewind-channel hand-off without pinning private helpers.
  - Smaller fixes: stale module doc comment removed; `simulate_artifact` edit branch flattened with guard clauses; `pick_failure_with_budget` walks reviewer slots by index instead of cloning a `Vec<String>` of names; `failures` collected as `Vec<&Verdict>`. `RunnerCtx.review_state` field deleted (state moved to `run_agent_loop` local).
  - 52 unit tests pass overall (was 47).
- [x] **Sidebar UI + live in-chat review indicator**. Adds `ListPrinciples` / `SetPrinciples` request variants.
  - Protocol landed: `ChatRequest::{ListPrinciples, SetPrinciples{names}}` at variant indices 13/14, `ChatOk::Principles{principles: Vec<PrincipleInfo>}` at 10. `PrincipleInfo {name, title, description, persona, applies_to}` projects `Principle::list` to the picker. Engine handlers wired in `engine.rs` (`SetPrinciples` writes through `save_state` and broadcasts `StateChanged`, mirroring `SetPersona`). TS package (`packages/principled-protocol`) extended with matching encode/decode arms; Rust + UI golden tables both updated. `Principle::list` `#[allow(dead_code)]` removed.
  - Picker UI landed: new `PrinciplesPicker` (chip + popover with checkboxes and per-row up/down priority arrows) wired into `PersonaComposer` next to the persona dropdown. `SessionSnapshot` gained a `principles: string[]` slot synced from `subscribed`/`stateChanged`/`stateUpdated`. App refetches `ListPrinciples` on mount and on each send (parallel to personas). Reducer now handles the `principles` ChatOk variant as a no-op (App owns the list). Type-check + golden tests (29 pass) + UI build all green.
  - Sidebar + in-chat indicator landed: `session.ts` gained `activeReviews: Record<stepId, ActiveReview>` and `reviewLog: ReviewLogEntry[]` slices fed by the five `review*` events. `reviewFrameOpened` pushes a frame with attempt `1/1`/empty-blocking; `reviewFrameProgress` overwrites attempt/max/blocking and resets the per-attempt verdict map; `reviewerCompleted` stores the latest verdict per principle and appends an entry to `reviewLog` (persona blank for live rows — denormalized into the sidecar but not the wire event); `reviewFrameResolved` deletes the frame. The `reviews` ChatOk response replaces `reviewLog` wholesale on Subscribe. `adapter.ts::appendActiveReviews` projects each active frame as a `kind: "system"` chat-widget bubble with the line `reviewing <tool>(<args>) · attempt N/M · [blocking…]` so the chat surfaces the in-flight gate inline. `App.tsx` fetches `ListReviews` after Subscribe (parallel to `GetMetrics`/`ListPersonas`/`ListPrinciples`) and wraps `<ChatView>` in a flex shell with `<ReviewerSidebar>` (320 px fixed, scrollable, filterable by verdict bucket). Component file `ReviewerSidebar.tsx` + module CSS, no new tests yet (pure rendering; existing `session.test.ts` reducer cases cover the slice updates implicitly through reduce-style action handling).

### Reviewer surface design

**Events (engine → UI broadcasts).** Two granularities so the sidebar gets a full audit without forcing the in-chat indicator to diff a roster on every transition:

```
ReviewFrameOpened   { step_id, tool_name, args_summary }                     // pre-exec: chat inserts placeholder
ReviewerStarted     { step_id, attempt_id, principle, persona }
ReviewerCompleted   { step_id, attempt_id, principle, persona, verdict_kind,
                      severity?, reasoning, suggested_fix?, ts }
ReviewFrameProgress { step_id, attempt, max_attempts, blocking: Vec<String> } // drives in-chat chip
ReviewFrameResolved { step_id, outcome: Accepted | Rewound{feedback} | Escalated{reason} }
```

`StepId` already exists in `step.rs`; thread it through `ReviewSession::begin_frame` and out to broadcasts. `attempt_id` (a `u64` allocated per reviewer call) uniques every individual reviewer LLM invocation — re-running the same principle in a later iteration gets a fresh `attempt_id` so the sidebar shows it as a distinct row.

**In-chat placeholder.** New transient `Message` variant in `session.ts`, e.g. `{ role: "reviewing", stepId, toolName, argsSummary, attempt, maxAttempts, blocking }`. Lives only in `Turn.flushed` keyed by `step_id`; never written to `transcript.json`. `ReviewFrameProgress` updates the same row in place; `ReviewFrameResolved::Accepted` drops the placeholder (the real `ToolCallStarted` that follows takes its slot); `Rewound` and `Escalated` also remove it (the audit trail is the sidebar). Click-to-expand surfaces the live reviewer roster (latest `ReviewerCompleted` per principle in this attempt).

**Persistence (`reviews.jsonl`).** Append-only sidecar in `<state_dir>/reviews.jsonl`. One `ReviewLogEntry` per line:

```rust
ReviewLogEntry {
    ts: String,                    // RFC3339
    step_id: u64,
    attempt_id: u64,
    principle: String,
    persona: String,
    tool_name: String,
    args_summary: String,          // truncated engine-side (~120 chars)
    verdict: VerdictKindWire,      // Pass | PassWithNit{reasoning}
                                   //   | Fail{severity, reasoning, suggested_fix}
}
```

Engine writes one line per `ReviewerCompleted` event (same data, plus `ts` and `tool_name`/`args_summary` denormalized so the sidecar is self-contained). No pruning — the file grows for the lifetime of the session.

**`ListReviews` request.** New `ChatRequest::ListReviews` returns `Vec<ReviewLogEntry>` from the sidecar; UI calls it once after `Subscribe` (parallel to `GetMetrics`) so late-joining clients see the full history. Live updates ride the `ReviewerCompleted` broadcast — the UI appends to its in-memory log on each event.

**Sidebar UI.** New `ReviewerSidebar.tsx` component. Renders rows in wall-clock order (i.e. file order — already chronological since the engine appends as events fire). Filterable client-side by reviewer persona, principle, verdict, and step id. Clicking a row scrolls/highlights the matching tool call in chat (or shows "abandoned (rewound)" if the step's resolved state is `Rewound`/`Escalated` — which the UI infers from the absence of a `ToolCallCompleted` for that `step_id` paired with a `ReviewFrameResolved::Rewound|Escalated`).

**Open implementation order.**
1. ~~`step_id` plumbing: emit it on every `ReviewSession::begin_frame` + reviewer call.~~
2. ~~New `ChatEvent` variants (Rust + TS protocol + golden tests).~~
3. ~~Engine emits the events from `review.rs` / `reviewer.rs`.~~
4. ~~`reviews.jsonl` writer + `ListReviews` request.~~
5. UI: in-chat placeholder (smaller, stand-alone — visible win without sidebar plumbing).
6. UI: `ReviewerSidebar` component (depends on `ListReviews` round-trip + the reviewer events).

**Sidecar slice (4) landed.** `ReviewLogEntry { ts, step_id, reviewer_call_id, principle, persona, tool_name, args_summary, verdict }` mirrors the wire shape; `review::append_review_log` writes one JSON line per reviewer verdict to `<state_dir>/reviews.jsonl`. **Persist-before-broadcast**: the append runs first, and only on success is the matching `ChatEvent::ReviewerCompleted` sent — a failed disk write suppresses the live event, so late joiners (who replay via `ListReviews`) and current subscribers can't see different histories. Errors return `io::Result<()>` (serde failures folded via `Error::other`); the call site treats failure as "no broadcast." `review::reviews_log_path(state_dir)` is the canonical accessor. `ChatRequest::ListReviews` (idx 15) replays the file line-by-line; unparseable rows are skipped with a warning rather than failing the whole request, so partial-line corruption doesn't poison history. `ChatOk::Reviews { reviews }` (idx 11) returns the projected vec. `state_dir: PathBuf` threaded onto `ReviewApproval` and through `review::build`. Concurrent appends from the parallel reviewer fan-out rely on POSIX `O_APPEND` atomicity (writes ≤ PIPE_BUF / 4 KiB on Linux are atomic, and our JSON lines fit); a single-writer-via-channel refactor is deferred unless lines start crossing that bound. TS `principled-protocol` extended with the matching encode/decode arms + `ReviewLogEntry` interface; session reducer treats `reviews` as a no-op (App will own the audit log, parallel to personas/principles). Rust + TS golden tables both updated. 52 Rust unit tests + 39 TS tests pass.

**Events slice (1+2+3) landed.** Five new `ChatEvent` variants (`ReviewFrameOpened`, `ReviewerStarted`, `ReviewerCompleted`, `ReviewFrameProgress`, `ReviewFrameResolved`) at indices 10–14, with supporting wire types (`ReviewVerdictWire`, `ReviewSeverityWire`, `ReviewResolution`). `BeginOutcome.is_new` drives one-shot `ReviewFrameOpened`; an `AtomicU64` on `ReviewApproval` allocates a fresh `reviewer_call_id` per reviewer LLM call (Relaxed ordering — ids only need uniqueness). `ReviewSession` and `ReviewApproval` both hold a `broadcast::Sender<ChatEvent>` clone — `decide` emits Opened/Started/Completed; `apply_verdicts` (the runner-side single writer of the stack) emits Progress/Resolved (Accepted on Allow/AllSkipped, Rewound on Rethink, Escalated on AskUser). `FailureChoice::Retry` carries `attempt`/`max_attempts` so Progress reflects the picked principle's budget. Args are projected via a 120-char single-line `summarize_args`.

**Post-review fixes (29-principle pass):** renamed `attempt_id` → `reviewer_call_id` to avoid collision with the per-principle retry `attempt` carried on Progress; dropped redundant `persona` field from `ReviewerStarted`/`Completed` (chrome joins via `ListPrinciples`); `apply_verdicts` takes `&str` for tool name + args JSON; `Ordering::Relaxed` justified inline; the three `decide_*` behavioral tests now subscribe to the broadcast and pin the expected event sequence per case (pass → `[Opened,Started,Completed,Resolved::Accepted]`; fail → `[…, Progress{attempt:1/1,blocking:[p]}]`; rethink → `[…, Resolved::Rewound{feedback}]`). Rust + TS golden tables both updated; 7 + 52 + 36 tests pass.
- [x] **Two example principles** checked in. `principles/prefer-vec-over-hashmap.toml` and `principles/guard-clauses-over-nesting.toml` at the repo root, with descriptions condensed from the in-tree `../software-principles/` essays. Both target `applies_to = ["edit", "write"]`, request `tool_call + tool_artifact` context, and are driven by a new `personas/reviewer.toml` (low-temp Qwen, JSON-only system prompt). `lutin-control-panel/src/defaults.rs::seed` now seeds `<global>/principles/<name>.toml` alongside `<global>/personas/<name>.toml`, `include_str!`-ing both principle TOMLs and the new reviewer persona from the repo root. Test `seed_creates_all_defaults` extended to assert every principle file lands. `cargo check -p lutin-control-panel --tests` clean.

- [x] **Post-v1 hardening pass** (review against the in-tree principles themselves):
  - **`reviewLog` hydration race fixed**: `applyResponse(case "reviews")` now merges by `(stepId, reviewerCallId)` instead of wholesale replace, so live `reviewerCompleted` events that land between Subscribe and ListReviews aren't wiped.
  - **`ReviewLogEntry.persona` widened to `Option<String>` / `string | null`**: Rust + TS wire types updated, postcard golden bytes adjusted (extra `0x01` Some-tag byte). `null` = synthesize-from-live (chrome resolves via `ListPrinciples`); `Some` = recorded at sidecar write time. Drops the empty-string sentinel from `session.ts`.
  - **`activeReviews` shape: `Record<stepId, …>` → `ActiveReview[]`**: stringified u64 keys reorder under `Object.keys`; an array preserves insertion order and matches the `prefer-vec-over-hashmap` principle we ship as a default.
  - **`ActiveReview.maxAttempts` removed**: per-principle retry budgets mean any aggregate at frame-open time is a guess. Replaced with `progress: { attempt, maxAttempts } | null`; chip renders "attempt 1" pre-progress and "N/M" once `ReviewFrameProgress` carries a real per-principle pair.
  - **Exhaustiveness via `Record<RowBucket, string>`**: dead `default:` arm in `verdictClass` deleted in favour of a record lookup the type-checker enforces.
  - **`seed_dir` un-abstracted** back into two inline loops (rule of three: only two callers).
  - 7 + 52 Rust unit tests, 39 TS tests, and `cargo check -p lutin-control-panel --tests` all clean.

## Out of scope for v1 (post-v1 wishlist)

The v1 plan is closed. The shape is right for "shaped"; the items below are what would make it ready for daily-driver use, in rough order of cost / payoff:

- **`rewindable = false` per-tool flag.** The plan already calls out POSTs / real Bash as a hazard when a downstream `rethink` pops past them. Add one field to `Principle` (or a per-tool-kind config), one branch in `perform_rewind` that escalates to the user instead of popping past a non-rewindable accepted step. Smallest meaningful addition.
- **Verdict caching.** Worst-case cost is `principles × full re-review`; cache `(principle, args_hash, artifact_hash)` → previous verdict and skip stable passers within the same step's loop. Cheap, addressable inside `review.rs`.
- **Principle authoring UI.** TOML editor in chrome. Mostly UX; no engine work beyond a write-through `SetPrinciple` mirror of `SetPersona`.
- **Real git integration.** Replace the in-memory `Snapshot { files }` rewind primitive with a per-step commit. Bigger lift; opens the door to tree branching later.
- **Tree branching beyond linear stack** (`StepStack` → `StepTree`).
- **Dangerous-tool sandboxing** (run unreviewed `bash` / `web_post` in a sandbox so accepted-then-rewound side effects can be undone properly).

## Implementation status (snapshot)

All v1 tasks (1–8) plus a post-v1 hardening pass have landed. Architecture: review state is single-writer, owned by the runner task; the policy talks to it over an mpsc channel and oneshot replies. `Verdict` and `AttemptRecord` carry only legal states (no `pass + Fix` or `Accepted + !executed`). Denials wear a `<review-deny>` tag so the squash pass can't confuse them with genuine in-tool errors. Policy install failures end the turn loudly. UI surfaces an inline "reviewing tool · attempt N/M · K blocking" placeholder while a frame is in flight and a 320 px `ReviewerSidebar` to the right of `ChatView` showing the persisted reviewer audit log (merged from `ListReviews` replay + live `reviewerCompleted` events). Two example principles + a `reviewer` persona ship via `lutin-control-panel`'s default seeder. 7 + 52 Rust unit tests + 39 TS tests pass; `cargo check -p lutin-control-panel --tests` and `bun run build` are clean. End-to-end smoke run with a real provider has not been done yet — that's the next thing to verify before picking up post-v1 work.
