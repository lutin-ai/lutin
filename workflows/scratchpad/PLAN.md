# `workflows/scratchpad` — design plan

A sibling workflow to `principled`. Same review-by-principles concept, different runtime model: the agent shapes a known-good action through guided iteration before any tool executes. Today's `principled` is **reactive** — agent acts, gets blocked, reframes. Scratchpad is **constructive** — agent and reviewers collaborate on a draft, then execute once when it's ready.

`principled` keeps running. Users pick per session. Both can coexist for as long as we want — there's no migration.

## Why

Three problems with `principled` that motivate this:

1. **Agent doesn't know it's iterating.** Denied tool calls look like failed tool results. The agent infers "I'm in a review loop" from context shape, badly. Sequential structured feedback on a draft removes the guesswork.
2. **Parallel reviewer fan-out produces incoherent feedback.** N reviewers fire concurrently and the agent gets a pile of contradictory suggestions. One-at-a-time sequential evaluation surfaces conflicts cleanly and lets the agent fix one thing at a time.
3. **Rewind machinery is the source of most bugs** (`step.rs`, `perform_rewind`, two sources of truth for review state). If iterations happen on a scratchpad _before_ any side effects, there's nothing to roll back. The whole rewind path disappears.

## Terms

- **Step.** One finalized tool execution. Begins with PLAN, ends with EXECUTE + SUMMARIZE.
- **Scratchpad.** The mutable proposal-in-progress for one step. Holds the plan, the chosen tool, the tool args, and the iteration history. Discarded after EXECUTE.
- **Iteration.** One pass through the principle list against the current scratchpad. Each iteration ends in either _all-pass_ (proceed to execute), _fix_ (agent edits scratchpad, re-iterate), or _switch_tool_ (back to PLAN with new intent).
- **Plan.** Structured intent for the step: `{tool, goal, why_this_tool, considerations}`. Produced by the `plan` tool at PLAN stage. Pinned for the duration of the step.
- **FixLog.** Last N (= 3) satisfied fixes for the current scratchpad. Shown to the iteration agent so it can synthesize across constraints rather than oscillate.
- **StepSummary.** Structured artifact produced after EXECUTE. Feeds the next step's context.
- **Principle.** Same concept as `principled`: a reviewer-LLM-driven gate. Tagged with `kind`, `required`, `points`.

## State machine

```
            ┌─────────────────────────────────────┐
            │                                     │
            ▼                                     │
┌──────────────────────┐                          │
│      PLAN            │   agent calls `plan`     │
│  (forced tool call)  │   tool → {tool, goal,    │
└──────────┬───────────┘   why_this_tool}         │
           │                                      │
           │ build Scratchpad with plan +         │
           │ initial tool args                    │
           ▼                                      │
┌──────────────────────┐                          │
│   SCRATCHPAD ITERATE │                          │
│                      │                          │
│  for principle in    │                          │
│  WORKFLOW_ORDER:     │                          │
│    verdict = run()   │                          │
│    if fix:           │   agent.edit_args()      │
│      edit, restart ──┘                          │
│    if rethink:       ──── agent.switch_tool() ──┘
│      back to PLAN
│    if pass: continue
│  all-pass → EXECUTE
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│      EXECUTE         │   tool actually runs;
│                      │   output captured
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│     SUMMARIZE        │   summarizer LLM produces
│                      │   StepSummary; appended to
│                      │   context for next step
└──────────┬───────────┘
           │
           ▼
      (next PLAN)
```

The agent must call `plan` exactly once. The output seeds the Scratchpad. Plan-stage reviewers (principles with `kind=Plan`) fire before scratchpad construction; they gate the _choice_ itself ("right tool for this goal?"). On `Fix` or `Rethink` at this stage, plan is re-run with the feedback included in the next prompt.

## Scratchpad iteration

The iteration agent is a _separate_ LLM call from the main agent. Its context is **only**:

- The Plan (pinned).
- The current Scratchpad args (rendered as the tool call it represents).
- The current principle's feedback.
- The FixLog: the last 3 satisfied requirements, stated as positive constraints.
- The pinned user goal.
- (No verbatim history — the iteration agent is task-scoped.)

Its tools are exactly:

```
edit_args(new_args: object)
switch_tool(new_tool: string, new_intent: string)
abort(reason: string)
```

There is no `finalize`. Finalization is automatic: when the principle list iterates from start to end without firing a Fix or Rethink, the scratchpad is finalized and EXECUTE runs.

**Sequential evaluation, ordered:**

- Principles iterate in `WORKFLOW_ORDER` (the existing static list). Plan-kind comes first by convention, impl-kind next, review-kind last; the order is human-curated, not derived from `kind`.
- After any Fix that produces an edit, evaluation **restarts from the first principle**. Necessary for soundness: an edit satisfying principle K can break principle 1.
- Optional optimization (later, only if needed): memoize verdicts by `args_hash`. Skip principles whose verdict is still valid for the current hash.

**FixLog presentation:**

When principle K returns Fix on iteration N, the iteration agent receives:

```
You are refining a planned tool call. Recently satisfied requirements:
  • [principle A]: <one-line requirement>
  • [principle B]: <one-line requirement>
  • [principle C]: <one-line requirement>

Current concern from [principle K]:
  <feedback>

Edit the parameters to address the concern while keeping the above satisfied,
or switch the tool if the concern means this is the wrong approach.
```

No raw iteration count is shown. Framing emphasizes refinement, not failure.

## Principle library

Existing TOML schema extended:

```toml
name = "plan-before-edit"
title = "Plan before editing"
description = "..."
kind = "plan"           # plan | impl | review
required = true         # if false, points must be set
points = null           # u8 proportional to importance when budgeted
applies_to = { ... }    # tool/persona filters (unchanged)
persona = "reviewer-rust"
```

`kind` is a tag for UI grouping and validation (assert plan-kind precedes impl-kind in `WORKFLOW_ORDER`). Runtime order is still the explicit list in `principles.toml`.

`required = true` → must-have. No budget. Cannot be skipped.
`required = false` → budgeted. `points` is the per-step budget. Each Fix verdict costs one point. At 0 points, principle is skipped for the rest of this step.

## Conflict resolution — defense in depth

Four layers, each catching what the prior lets through:

1. **Principle hygiene (design time).** Conflicting principles get merged or rephrased. Most conflicts shouldn't exist.
2. **FixLog (iteration time).** Agent sees the last 3 satisfied requirements and synthesizes across them rather than oscillating.
3. **Points (run time).** When synthesis is genuinely impossible, importance-proportional budgets pick a winner. Budgeted principles exhaust; required principles outlast them.
4. **Max-retries fallback (terminal).** See next section.

**Oscillation detection** is a complement to (3): track `args_hash` per iteration. If the current hash matches the hash from two iterations ago, you've cycled. Surface to the iteration agent as part of the prompt: _"The args have cycled — pick one approach and commit, or switch the tool."_ Cheap (a hashmap), catches conflicts before points exhaust.

## Max-retries behavior

Two thresholds, two response shapes:

**Per-principle attempt cap (e.g., 5 attempts on one principle):**

- **Budgeted principle** at cap → bypass for this step, append to `bypassed_principles` in the StepSummary, emit `ChatEvent::PrincipleBypassed`.
- **Required (must-have) principle** at cap → force `switch_tool`. Iteration continues against the new plan. Emit `ChatEvent::PersistentMustHaveFailure { principle, attempts }` so the UI can show a non-modal banner: _"Agent has tried 5 times and is failing must-have 'X'. Watching."_

**Absolute per-step iteration cap (e.g., 50):**

- Hard stop. Step ends with `MessageFinished{Failed("iteration cap reached")}`. Runaway-cost protection. The cap should be high enough that hitting it really means something's broken.

**Default behavior: keep iterating with observable signals.** Never silently fail on must-haves; never block on user prompts unless the user opted in. A persistent failure surfaces visibly but does not stop the agent — the user has `Cancel` if they want to intervene.

**Configurable per session:**

```toml
# state.toml
max_retries_action = "keep_going_with_signal" # default
# or "stop_on_must_have_failure" for production
# or "ask_user" for fully-interactive
absolute_iteration_cap = 50
per_principle_attempt_cap = 5
```

## Execute stage

Once the scratchpad iterates to all-pass, the tool runs. Standard tool dispatch — same toolbox as `principled`. Output is captured into the step's record.

No `ApprovalPolicy` here. Approval already happened during iteration. The SDK's approval hook is unused (or trivially-Allow) for this workflow.

## Summarize stage

One LLM call after EXECUTE. Input: the Scratchpad (plan + args + iteration history) + tool output. Output: structured `StepSummary`.

```rust
ToolDef {
    name: "submit_summary",
    description: "Summarize the step that just completed.",
    input_schema: {
        args_digest: string (≤200 chars),
        output_digest: string (≤200 chars),
        key_facts: [string],  // ≤5 entries, each ≤200 chars
    }
}
```

The summarizer model is small/fast (configurable per persona; defaults to the persona's main model with a low-temp override). Structured output, not free-form prose. Reduces summary rot.

**Laziness:** Run summarizer _only when the verbatim window is about to be evicted_. Default window = 10 messages. While the window has room, recent steps stay verbatim and no summarizer call fires. When the window evicts message M, summarize M's step lazily then. Saves an LLM call per step on short sessions; preserves recent fidelity for free.

## Context model

Every agent call (PLAN, ITERATE, SUMMARIZE) builds context as:

1. **System prompt** (persona-derived, unchanged from `principled`).
2. **Pinned user goal** — the most recent `Message::User` from the user, verbatim. Always present. Anchors session intent across many steps.
3. **Step summaries** — `StepSummary` records for the N most recent evicted steps (default N=20, configurable). Rendered as structured text:
   ```
   [step 7] tool=edit goal="add bounds check to parse_input"
     args: replaced lines 12-18 with guard-clause structure
     output: applied; 3 lines added
     key_facts:
       - parse_input is called from handler.rs:42 and util.rs:88
       - existing tests in parse_test.rs cover empty + over-long input
   ```
4. **Verbatim window** — the most recent 5-10 raw `Message`s, unsummarized. Captures mid-thought reasoning.
5. **Iteration-specific addendum** (ITERATE only): plan + scratchpad args + FixLog + current concern.

The user's seed `Message::User` is _never_ evicted — it stays pinned. Step summaries beyond the recent-N cap roll off silently (still on disk in the session's `summaries.jsonl`).

## The `explore` tool

A free-form info-gathering tool the _main_ agent (at PLAN stage) can call instead of `plan`. Same shape as today's `Read` etc., but explicitly tagged as exploration:

```rust
ToolDef {
    name: "explore",
    description: "Gather information before planning. Use when you need \
                  more context to decide which tool to run next.",
    input_schema: { what: string, why: string }
}
```

Internally `explore` dispatches to a small set of read-only operations (Read, Grep, ListDirectory). The result feeds straight into the PLAN stage's next call — no iteration, no review (one dedicated `explore-safety` plan-kind principle gates it to prevent secret leakage).

This is the escape valve for "I don't know enough to plan yet." Without it, the agent's only option is to `plan` a poorly-justified tool and let reviewers reject it — expensive and roundabout.

## UI surface

Three new event types to broadcast:

- `ChatEvent::PlanProposed { step_id, plan }` — emit at PLAN end. UI shows the agent's intent as a bubble _before_ any tool args are visible. User sees _what the agent is going to do_ before it does it.
- `ChatEvent::ScratchpadUpdated { step_id, iteration, args, current_principle, verdict }` — emit at every iteration boundary. UI shows live progress through the principle list ("✓ guard-clauses · ✓ handle-errors · ⟳ minimize-defensive · ☐ explain-changes").
- `ChatEvent::PersistentMustHaveFailure { step_id, principle, attempts }` — emit at threshold. Non-modal banner.

The audit log (`reviews.jsonl` equivalent — call it `steps.jsonl`) stores one record per finalized step with the full IterationRecord history. Sidebar can replay any past step's iterations as a timeline.

## Tools the iteration agent does _not_ have

Explicitly excluded from the iteration agent's toolbox:

- The main toolset (read, write, edit, shell, etc.) — those run only at EXECUTE.
- `finalize` / `approve` / `commit` — finalization is automatic.
- Any tool that performs side effects on disk or external systems.

Iteration is a pure proposal-shaping phase. Side effects only at EXECUTE.

## Non-goals (v1)

Things we are _not_ designing now, to keep scope contained:

- **Sub-agents within a scratchpad iteration.** A scratchpad step is single-agent. Sub-agent orchestration is `principled`'s job for now.
- **Multi-step plans.** Each PLAN call decides one tool. A "do these three things in order" plan is a future feature; today the agent just runs three steps.
- **User-in-the-loop iteration.** No "ask the user mid-iteration" mode. `ask_user` as a `max_retries_action` is a v1.1 thing.
- **Cost optimization (model routing, principle skipping).** Build the system first, measure, then optimize. Hash-memoized re-evaluation is a placeholder for "later."
- **Cross-session memory.** Same boundary as `principled` — each session is self-contained.
- **Migration tool from `principled` sessions.** They cohabit; users start new sessions to use scratchpad.

## Relationship to `principled`

- Sibling crate under `workflows/scratchpad/`. Own Cargo.toml, own binary, own state dir layout.
- Both workflows can be spawned by `lutin-project` simultaneously; the user picks per-session at session creation time.
- Shared dependencies (`lutin-agent-sdk`, `lutin-workflow-sdk`, `lutin-tools`, principle TOMLs, persona TOMLs) stay in their existing crates. Each principle TOML grows two optional fields (`kind`, `required`/`points`); when missing, principled ignores them and scratchpad treats them with sane defaults (`kind = "impl"`, `required = false`, `points = 3`).
- No code is shared at the workflow level — the runtime models are different enough that abstracting across them would be worse than the duplication.

## Worked example — "add bounds check to `parse_input`"

User message: _"Add a bounds check to parse_input so it rejects empty strings."_

**Step 1 — PLAN.**

Agent's context: pinned user goal + empty summaries + verbatim window (just the user message).

Agent calls `plan`:

```json
{
  "tool": "Edit",
  "goal": "Add an early return rejecting empty input to parse_input",
  "why_this_tool": "Targeted modification to one function in one file",
  "considerations": [
    "Preserve existing tests",
    "Match project's guard-clause style"
  ]
}
```

Plan-kind principles run sequentially. `plan-before-edit` checks: ✓ (plan exists). Plan stage completes.

**Step 1 — SCRATCHPAD ITERATE.**

Scratchpad seeded with Plan + initial args (agent's draft):

```json
{
  "path": "src/parse.rs",
  "old_string": "fn parse_input(s: &str) -> Result<Value> {",
  "new_string": "fn parse_input(s: &str) -> Result<Value> {\n    if s.is_empty() {\n        return Err(anyhow!(\"empty input\"));\n    }"
}
```

Iteration 1: `guard-clauses-over-nesting` runs. Verdict: Pass.
Iteration 1 continues: `handle-errors-explicitly` runs. Verdict: Fix — _"`anyhow!` strings should reference the function name for traceability"_.

Iteration agent sees:

```
Recently satisfied: (empty)
Current concern from handle-errors-explicitly: anyhow! strings should reference the function name for traceability
```

Agent calls `edit_args` with `anyhow!("parse_input: empty input")`. FixLog records: _handle-errors-explicitly: error strings include function name_.

Iteration 2 restarts from principle #1. `guard-clauses-over-nesting` ✓. `handle-errors-explicitly` ✓ (now). `minimize-defensive-code` runs. Verdict: Pass (this is a real expected case, not speculative defense).

All principles pass. Scratchpad finalizes.

**Step 1 — EXECUTE.** `Edit` tool runs. 3 lines added.

**Step 1 — SUMMARIZE** (deferred until verbatim window fills).

When eventually summarized:

```json
{
  "tool": "Edit",
  "plan": { "goal": "Add an early return rejecting empty input to parse_input", ... },
  "args_digest": "src/parse.rs: added guard for empty input at top of parse_input",
  "output_digest": "applied; 3 lines added at line 8",
  "key_facts": [
    "parse_input now returns Err on empty string with traceable message",
    "guard-clause style; no nesting"
  ],
  "satisfied_principles": ["guard-clauses-over-nesting", "handle-errors-explicitly", "minimize-defensive-code"],
  "bypassed_principles": [],
  "iterations": 2
}
```

That summary is what the next step sees in its context — three lines instead of the full iteration trace.

## Implementation order (rough)

1. **Crate skeleton.** Mirror `workflows/principled/` layout. Cargo.toml, lib.rs (wire types), engine.rs bootstrap.
2. **Data types.** All structs from §"Data types" above. With serde.
3. **PLAN stage.** `plan` tool, plan-stage iteration. Stub out reviewers initially.
4. **Reviewer adapter.** Reuse `principled`'s `reviewer.rs` (the verdict-parsing + retry logic is sound). Wire it to sequential evaluation instead of parallel fan-out.
5. **Scratchpad iteration loop.** Sequential principle dispatch, FixLog, args hash, oscillation detection.
6. **Iteration agent prompting.** The three-tool agent (edit_args, switch_tool, abort).
7. **EXECUTE wiring.** Standard tool dispatch, output capture.
8. **Summarize stage.** Structured summarizer call, laziness.
9. **Context builder.** Pinned goal + summaries + verbatim window.
10. **Persistence.** `steps.jsonl`, `summaries.jsonl`, `state.toml`.
11. **WS protocol + UI events.** PlanProposed, ScratchpadUpdated, PersistentMustHaveFailure.
12. **UI panel.** Live plan display, principle progress, persistent-failure banner. (Can mirror principled's chrome with a different sidebar.)
13. **Principle library extensions.** Add `kind`, `required`, `points` to existing TOMLs. Sane defaults for unannotated principles.

Steps 1–6 produce a usable system (can iterate against a real session); 7–10 make it useful; 11–13 make it pleasant.

## Open questions

- **Plan-kind principle gating on `explore` calls.** How tight should the safety principle be? Probably "don't read paths matching `.env*`, `*secret*`, `*credential*`." Start with a strict allowlist, loosen via user complaints.
- **Summarizer model.** Use the persona's main model with low temp, or a dedicated small model? Start with persona main; add per-persona override later.
- **Default `points` budget.** 3? 5? Probably 3 — small enough that exhaustion is meaningful, large enough that the agent gets a few chances to synthesize.
- **What counts as a "fix" for FixLog purposes?** Only Fix verdicts that produced an `EditArgs`. Aborts and switches don't contribute. (Switches reset the FixLog entirely — new tool, new constraints.)
