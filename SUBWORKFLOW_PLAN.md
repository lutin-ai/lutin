# Sub-agent / sub-workflow plan

Goal: let a workflow spawn background work and expose it to the LLM via tools,
with live status reflected into the system prompt so the launching agent
always knows what's running.

Two distinct mechanisms, deliberately not unified:

| Mechanism      | Scope                          | Process model              | Communication             |
|----------------|--------------------------------|----------------------------|---------------------------|
| In-workflow    | Chat: spawn another agent loop | Same process, tokio task   | In-process message passing|
| Cross-workflow | Any-launches-any (later)       | Separate proc / Docker     | Control-protocol via CP   |

Phase 1 is genuinely lightweight — just another agent loop in the same
process, full read access to in-memory state. Phase 2 is the heavier,
isolated, cross-workflow case. Tool naming and registry-shape symmetry
between the two will be decided after phase 1 is in use.

---

## Phase 1 — In-workflow sub-agent (chat workflow)

### Concept

A sub-agent is *not* a second engine instance. It's a second `Agent` (from
`lutin-agent-sdk`) running as a tokio task inside the chat workflow process,
with read access to the parent's in-memory state. No protocol messages, no
event translation, no broadcast pipelines.

### Design rule: message passing over shared mutable state

State that crosses task boundaries lives in **one owner** and is reached via
channels. The registry is an actor; spawned agent tasks send updates to it
via mpsc; the chat engine talks to the registry via mpsc + oneshot replies.

`Arc<Mutex<...>>` / `Arc<RwLock<...>>` are explicitly avoided. The one
permitted use of `Arc` is sharing **immutable** data (e.g. a transcript
snapshot the child reads) — `Arc<Vec<Message>>`, no interior mutability.

This matches the existing chat engine pattern (`engine.rs:128` —
`agent_cmds: mpsc::UnboundedSender<AgentCmd>`).

### Components

#### 1. `workflows/chat/src/agents.rs` — the registry actor

```rust
// IDs and value types
struct AgentId(Ulid);

enum AgentStatus { Running, Completed, Failed { reason: String }, Stopped }

struct AgentSpec {
    initial_prompt: String,
    persona: Option<PersonaId>,        // None = inherit parent's
    transcript_snapshot: Arc<Vec<Message>>, // immutable share
}

struct AgentOutcome {
    final_text: String,
    // room to grow: usage, tool calls, etc.
}

struct AgentSummary {
    id: AgentId,
    status: AgentStatus,
    last_progress: Option<String>,    // truncated, for system prompt
}

// Commands from chat engine / tools
enum AgentRegistryCmd {
    Spawn    { spec: AgentSpec,                 reply: oneshot::Sender<AgentId> },
    Status   { id: AgentId,                     reply: oneshot::Sender<Option<AgentStatus>> },
    Stop     { id: AgentId,                     reply: oneshot::Sender<()> },
    Snapshot {                                  reply: oneshot::Sender<Vec<AgentSummary>> },
}

// Updates from spawned agent tasks
enum AgentUpdate {
    Progress  { id: AgentId, last_text: String },
    Completed { id: AgentId, outcome: AgentOutcome },
    Failed    { id: AgentId, error: String },
}

// Internal slot — single owner (the registry actor)
struct AgentSlot {
    status: AgentStatus,
    progress: Option<String>,
    abort: AbortHandle,
    final_outcome: Option<AgentOutcome>,
}
```

The registry runs as a tokio task with `tokio::select!` between
`cmd_rx` and `update_rx`. All `AgentSlot`s live in its `HashMap`; nothing
is shared across tasks. Each chat engine instance has its own registry —
a child's registry is separate from its parent's, so children only see
their *own* children, never grandchildren via the parent's view.

### Completion delivery: "agent response" message + auto-turn

When the registry receives `AgentUpdate::Completed { id, outcome }` (or
`Failed`), it forwards a `CompletionEvent { id, outcome | error }` to the
chat engine via a dedicated channel (`mpsc::UnboundedSender<CompletionEvent>`
held by the registry, drained by the engine's main loop).

The chat engine's handler:
1. Appends an **agent response message** to its transcript. Format:
   a new message variant (or a tagged `Message` content) clearly attributed
   to the child by id, containing the child's `final_text`. Decide concrete
   shape during impl — likely a new `MessageRole::AgentResponse { agent_id }`
   variant in chat-protocol types, or an annotated assistant message.
2. Triggers the parent's agent loop to take a turn (auto-turn on completion).
   - If the parent is **idle** (waiting for user input): dispatch a turn
     immediately.
   - If the parent is **mid-turn**: append to transcript; the next turn
     decision after current turn picks it up. Do not interrupt or
     re-enter.

Re-entrancy guard: if multiple children complete during a parent turn,
their messages all append to the transcript; one auto-turn fires after the
current turn finishes, with all completions visible in context.

#### 2. Spawning an agent task

The registry's `Spawn` handler:
1. Builds a fresh `Agent` via a new helper `build_subagent(spec, parent_ctx)`
   in the chat engine. The helper reuses `lutin-workflow-sdk::agent` for the
   persona+settings → Agent path (whatever exists today).
2. Clones `update_tx` and the `AgentId`.
3. `tokio::spawn`s a small future:
   ```rust
   async move {
       let result = agent.run(initial_prompt).await;
       let _ = update_tx.send(match result {
           Ok(outcome) => AgentUpdate::Completed { id, outcome },
           Err(e)      => AgentUpdate::Failed { id, error: e.to_string() },
       });
   }
   ```
4. Stores the resulting `AbortHandle` in a new `AgentSlot`.

For progress, the spawned future taps the agent SDK's `AgentEvent` stream
(if accessible) and sends `AgentUpdate::Progress { last_text }` whenever
the assistant emits a new message. If that stream isn't conveniently
exposed, **skip progress in v1** and only show `Running` / `Completed` /
etc. in the system prompt — better than coupling deeply just for a status
line.

#### 3. Wiring into the chat engine

`workflows/chat/src/engine.rs` gains one field:
```rust
agent_registry: mpsc::UnboundedSender<AgentRegistryCmd>,
```
Started during engine init alongside the existing channels. Dropped on
engine shutdown — the registry actor exits its loop, which aborts all live
agent tasks (clean reaping, no Arc juggling).

#### 4. Refactor needed in chat engine

Today the engine builds **one** Agent at startup, entangled with broadcast
setup, history projection, etc. Extract a `build_subagent(spec, ctx) ->
Agent` function from that path. Expected size: 50-200 lines of moved code.

The function takes whatever the parent has (settings, tool context, HTTP
client) and returns a fully-configured `Agent` without touching the parent's
broadcast channels or transcript-of-record.

#### 5. System-prompt augmentation

Prompt builder gains a step that:
1. Sends `AgentRegistryCmd::Snapshot` to the registry, awaits the reply.
2. If non-empty, serializes a block:
   ```
   <active_subagents>
   - id=01HXY... status=running progress="searching for X"
   - id=01HXZ... status=completed
   </active_subagents>
   ```
3. Omitted when no agents are live.

#### 6. LLM-facing tools (chat-workflow-local)

Live in `workflows/chat/src/tools/agent.rs`. Each tool holds the
`mpsc::UnboundedSender<AgentRegistryCmd>` and a `oneshot` per call.

- `spawn_agent { initial_prompt, persona? } -> AgentId`
- `agent_status { id } -> AgentStatus`
- `agent_stop { id } -> ()`

No `agent_output` tool — child results arrive automatically as agent-response
messages in the parent's transcript on completion. If a deliberate "early
peek before completion" need shows up, add it then.

### Gating: which personas can spawn

`spawn_agent` (and `agent_status` / `agent_stop`) are added to a persona's
toolbox only when that persona is intended to delegate. Most personas won't
have them. There is no depth or fan-out cap in the registry — recursion is
allowed and intentional (an orchestrator can spawn another orchestrator).
Children only know about their direct parent and their own children;
information flows up one level at a time via agent-response messages.

### Stop-path semantics (state machine rules)

These belong as comments at the top of `agents.rs`:

- `Stop { id }` handler: synchronously sets `slot.status = Stopped` and
  fires `slot.abort.abort()`. Replies on its `oneshot` immediately.
- The aborted spawn future is cancelled mid-execution; it does **not**
  send `Completed` / `Failed` upstream.
- Late `AgentUpdate::Progress` from an aborted task may already be in the
  `update_rx` queue. The update handler **ignores updates whose slot is
  not in `Running`** — never resurrect or transition out of a terminal
  state.
- `Completed` / `Failed` updates also forward a `CompletionEvent` to the
  chat engine; `Stop` does **not** — the parent already knows it stopped
  the child, no agent-response message is generated.

### Open / decided points

| Question                        | Decision for v1                                     |
|---------------------------------|-----------------------------------------------------|
| Transcript snapshot shape       | Full snapshot, `Arc<Vec<Message>>`. No `compact`.   |
| Tool inheritance                | Same toolbox as parent (minus persona-gated tools). |
| Depth / fan-out limits          | None. Gating is via persona toolbox composition.    |
| Child can write parent state?   | No. Read-only via `Arc<Vec<Message>>`.              |
| Progress streaming              | If SDK exposes `AgentEvent` stream cleanly: yes. Else skip — status-only. |
| Auto-turn on completion         | Yes. Mid-turn completions queue; turn fires after current. |
| Agent-response message shape    | New role/variant in chat-protocol types — concrete shape decided during impl. |

### Build sub-order

0. **[DONE] Verify `Agent::run` API shape** — SDK is event-stream-based,
   not `run() -> Result`. Real shape: `Agent::start() -> Stream<AgentEvent>`
   then `Agent::join() -> RunOutcome`. The production spawn future will
   pump the event stream (translating `AssistantText`/`AssistantMessage`
   to `AgentUpdate::Progress`) and read `outcome.last_assistant` on join
   to build `AgentOutcome::final_text`. See `crates/lutin-agent-sdk/src/agent.rs`.
1. **[DONE]** Define value + channel types in `workflows/chat/src/agents.rs`.
2. **[DONE]** Implement registry actor.
3. **[DONE]** Unit-test the registry — 8 tests cover spawn/status/stop/
   late-update/completion/failure/snapshot/drop-reaps.

#### Refinements applied during 1–3 (deviations from the spec above)

- **`AgentId(u64)`** — monotonic counter newtype, not `Ulid`. Easier to
  display (`agent#7`), no extra dep. Swap to a UUID/Ulid type later if
  cross-process IDs are ever needed (phase 2).
- **`AgentSlot` is a state-bearing enum**, not a struct with parallel
  `status` + `final_outcome` fields. Variants:
  `Running { progress, abort }` / `Completed { outcome, progress }` /
  `Failed { reason, progress }` / `Stopped { progress }`. This makes
  "Completed without an outcome" and "post-terminal AbortHandle"
  type-impossible. Public-facing `AgentStatus` is derived via
  `slot.status()`.
- **`Spawner` callback type** (`Box<dyn FnMut(AgentId, AgentSpec,
  mpsc::UnboundedSender<AgentUpdate>) -> AbortHandle + Send>`) decouples
  the actor from `Agent` construction. The production wiring (step 5)
  plugs in a real spawner that builds an `Agent`, drives its event
  stream, and forwards updates. Tests pass a stub that captures
  `update_tx` and parks the child via `std::future::pending`. Without
  this seam, the unit tests would need a fully-built provider/persona/
  settings stack — disproportionate to what they verify.
- **Module has `#![allow(dead_code)]`** until step 5 wires it in. Remove
  the attribute as part of that step.
- **`truncate_chars` is duplicated with `engine.rs:868`.** Defer dedup to
  step 5 — at that point both call sites converge on a shared helper
  (likely in a new `text.rs` or moved into `lutin-workflow-sdk`).

4. **[DONE]** Refactor: extract `build_subagent` from `engine.rs`.
   - `resolve_args` now takes `persona_override: Option<&str>`; existing
     callers pass `None`. Persona priority is override → session state →
     `DEFAULT_PERSONA`.
   - `build_subagent(ctx, &AgentSpec)` lives next to `build_initial_agent`
     in `engine.rs`. It re-resolves args (so sub-agent settings track
     the parent's current on-disk config), seeds messages from
     `spec.transcript_snapshot` (cloned out of the `Arc<Vec<Message>>`),
     and pushes `spec.initial_prompt` as a `Message::User` so the
     spawner's `agent.start()` consumes it on round 0.
   - Returns `Result<Agent, String>` — sub-agent failures surface as
     `AgentUpdate::Failed { error }` to the registry, not as
     `ChatError`. Carries `#[allow(dead_code)]` until the step-5 spawner
     wires it in.
   - Smaller than spec'd (~25 lines, not 50–200): the existing
     `resolve_args` / `sdk_build_agent` / `map_build_error` factoring
     was already clean enough that there was nothing to disentangle —
     the new helper composes them rather than carving them out.
5. **[DONE]** Wire registry sender + completion receiver into the chat engine.
   - `RunnerCtx` now `#[derive(Clone)]` so the production spawner can
     own a clone (all fields were already `Clone`-friendly: `PathBuf`,
     `broadcast::Sender`, `Arc<Mutex<…>>`).
   - Registry boots inside `run_agent_loop` after the boot-summary
     write. Spawner closure captures a `RunnerCtx` clone and per-call
     spawns a `run_subagent_task` tokio task; returns its `AbortHandle`.
   - `RegistryHandles { cmd_tx, completions_rx }` retained on the
     runner stack: `cmd_tx` is `_agent_registry` (kept alive but unused
     until step 8 hands clones to LLM tools), `completions_rx` is
     drained in the runner's main `select!` arm — the handler is a
     `debug!` placeholder until step 6 wires the auto-turn.
   - The runner's `while let Some(cmd) = rx.recv().await` is now a
     `tokio::select! { rx.recv() / completions_rx.recv() }` loop. The
     `Cancel`/`Mutate`/etc. dispatch logic is unchanged inside it.
   - `run_subagent_task` translates the SDK event stream:
     `AssistantText` → `AgentUpdate::Progress`, `Error` → `Failed`,
     `Finished` → break + `agent.join().await`. Terminal mapping:
     `Stopped`/`MaxRounds` → `Completed { final_text }` (pulled from
     `outcome.last_assistant`); `Cancelled` is unreachable from the
     registry's `Stop` (abort kills the task pre-join), but if it ever
     fires we surface it as `Failed` so the parent sees something;
     other reasons → `Failed`.
   - Known v1 wart: `AbortHandle::abort()` cancels our outer task but
     `Agent` has no `Drop` cancel hook, so the agent's inner `drive()`
     task keeps running until natural completion (no provider request
     interruption). Documented in code; revisit if it shows up as a
     real cost.
   - Module-level `#![allow(dead_code)]` on `agents.rs` retained:
     experimentally removing it surfaced 5 warnings on surface that
     wakes up in steps 6–8 (`AgentRegistryCmd` variants, `AgentSummary`
     fields, `CompletionEvent` reads, `AgentSlot::Completed.outcome`).
     Updated the comment so it's clear what's still pending.
   - All 8 registry unit tests still green; no behavior changed under
     them.
6. **[DONE]** Engine handler appends agent-response messages to the
   transcript and triggers an auto-turn.
   - **Wire shape decision (v1):** kept the LLM-side message as a
     `Message::User(format!("[agent#N response]\n{text}"))` rather than
     introducing a new `lutin_llm::Message` variant or a new
     `HistoricalRole`. The new-variant path would touch every provider
     (`anthropic/messages.rs`, `openai_compat`, `ollama`, `openrouter`)
     plus the postcard wire + JS decoder + the golden-bytes table; the
     in-band marker is a one-line change that gets the orchestrator
     persona running. Documented as a known wart in
     `format_completion_message` — revisit when sub-agents leak into
     non-orchestrator personas.
   - `RunnerCtx` now carries `next_turn: Arc<AtomicU64>` (shared with
     `AppState`) so auto-turns draw `TurnId`s from the same monotonic
     source as user-driven turns. Added `RunnerCtx::next_turn()`.
   - Completion handler `handle_subagent_completion` lives next to
     `run_subagent_task`. It allocates a turn, `ensure_agent`s,
     `push_message`s the formatted text, persists + broadcasts
     `HistoryReplaced`, then calls `run_turn(ctx, rx, a, None, turn)`
     to kick the agent against the new transcript.
   - Mid-turn queueing is enforced naturally by tokio scheduling: the
     completion arm of the outer `select!` can't progress while
     `run_turn` is awaiting in the `Send`/`Rerun` arm, so completions
     accumulate on `completions_rx` until the current turn finishes.
     No explicit guard needed.
   - Failed completions follow the same path with an `[agent#N failed:
     <reason>]` text — the orchestrator's LLM gets to decide whether
     to retry, escalate, or give up. The registry-side `Stopped` slot
     transition correctly emits no `CompletionEvent`, so user-driven
     `Stop` doesn't trigger an auto-turn.
   - All 15 tests still green (8 registry + 7 chat-protocol golden).
     `#![allow(dead_code)]` on `agents.rs` retained — the remaining
     unused surface (`AgentRegistryCmd` variants, `AgentSummary`
     fields) wakes up in steps 7–8.
7. **[DONE]** System-prompt block + `Snapshot` command.
   - **Registry construction moved to `main()`** so `RunnerCtx` can
     own `agent_registry: UnboundedSender<AgentRegistryCmd>` from the
     start. Order: pre-create `(cmd_tx, cmd_rx)` and
     `(completions_tx, completions_rx)`, build `RunnerCtx` with a
     `cmd_tx.clone()`, clone `RunnerCtx` into the spawner closure,
     hand the receiving ends to a new
     `Registry::spawn_with_channels(cmd_rx, completions_tx, spawner)`.
     The original `Registry::spawn(spawner)` stays as the test entry
     point (now a thin wrapper around `spawn_with_channels`).
   - Cycle resolution: spawner closure captures a `RunnerCtx` whose
     `agent_registry` is the same `cmd_tx` the registry actor consumes
     from — which is exactly what we want, so each child sub-agent's
     tools (step 8) close over the *same* registry handle as the parent
     and can spawn grandchildren that show up in everyone's snapshot.
   - `run_agent_loop` signature gained `completions_rx`; the in-runner
     spawn block from step 5 is gone. `AppState` also gained
     `agent_registry` (currently `#[allow(dead_code)]`) so step-8 tools
     mounted at agent-build time can clone from there if needed.
   - `subagent_block(&RunnerCtx) -> Option<String>` issues
     `AgentRegistryCmd::Snapshot` over the cmd channel + a oneshot,
     returns `None` on empty/dropped registry (treated as "no block to
     inject"). Format mirrors the spec example:
     `<active_subagents>\n- agent#N status=… progress=…\n…\n</active_subagents>`.
     Terminal entries (Completed/Failed/Stopped) are kept in the block
     for audit-trail context — registry GC is not in v1.
   - `run_turn` augments the system prompt *after* `sdk_refresh_agent`
     via `agent.update_config(|cfg| cfg.system.push_str(…))`. Doing it
     post-refresh means the persona's prompt is the canonical input on
     every turn and the block can't accumulate stale entries — it's
     re-derived fresh per turn.
   - Auto-turns from sub-agent completions go through the same
     `run_turn`, so they too see an updated `<active_subagents>`
     block (now reflecting the just-completed child as `completed`).
   - All 15 tests still green; no test surface changes (`Registry::spawn`
     still works for them via the new wrapper).
7.5. **[DONE]** Mid-build review pass (10 sub-agent reviews against
   `software-principles/`). Concrete cleanups landed before step 8:
   - Dropped stale `#[allow(dead_code)]` on `build_subagent` (now
     called by `run_subagent_task`) and the redundant field-level
     allow on `AgentSpec` (already covered by the module-wide allow).
   - Dropped `agent_registry` from `AppState`: no current reader, and
     step-8 tools build inside the runner where `RunnerCtx` already
     carries the sender. Re-add to `AppState` only if a chat-protocol
     handler genuinely needs it.
   - Folded the speculative `Cancelled` arm in `run_subagent_task`
     into the catch-all `other =>`. `Cancelled` is unreachable from
     `Stop` (abort kills the task pre-join); keeping it as a separate
     arm with a fabricated "cancelled internally" string was dead
     code per the principle.
   - `resolve_args` now hard-errors when `project_config_dir.parent()`
     is `None` instead of falling back to `project_config_dir` itself.
     A None parent means a malformed env handoff (root or empty
     path); silently using `.lutin/` as the sandbox root would be
     worse than failing the turn loudly.
   - Deferred (lower-signal): `Option<String>` → named-enum migrations
     for `run_turn(text)` and `resolve_args(persona_override)`,
     `HashMap` → `Vec` for the registry slot map, comment trimming,
     and the `format_completion_message` save-loss documentation.
     Re-evaluate once step 8 surfaces real call patterns.
   - Test gaps (`handle_subagent_completion`, `subagent_block`,
     `run_subagent_task` terminal mapping, `build_subagent`) noted
     but not filled this pass — the engine-side wiring is hard to
     unit-test without a fake provider; revisit with end-to-end test
     in step 9.
   All 15 existing tests still pass.

8. **[DONE]** Implement the three tools, register them in personas
   designated as orchestrators.
   - **Tool names tracked the persona TOML, not the plan draft.**
     `personas/orchestrator.toml` was already authored with
     `spawn_agent` / `get_agent` / `stop_agent` in its whitelist;
     renaming `agent_status` → `get_agent` and `agent_stop` →
     `stop_agent` to match avoids a "plan rename" that would have
     touched user-edited files. (`list_agents` and `message_agent`
     from the persona's whitelist stay unimplemented for v1 — the
     `<active_subagents>` system-prompt block already covers
     enumeration, and `message_agent` requires a "resume the child
     with another turn" path the registry doesn't have yet.)
   - Tools live in `workflows/chat/src/tools/agent.rs`. Each holds a
     `mpsc::UnboundedSender<AgentRegistryCmd>` clone and replies via
     a per-call `oneshot` — same actor pattern as the rest of the
     workflow, no `Arc<Mutex>`. `make_subagent_tools(cmd_tx)` returns
     `Vec<Box<dyn Tool>>` so the SDK boundary matches `default_tools`.
   - Tools always go into `BuildArgs::extra_tools`; gating is the
     persona's `tool_filter_list`. To make this work,
     `lutin-workflow-sdk::build_inputs` had to flip its filter order:
     extra_tools now extends the default set *before* the filter
     runs, instead of being appended after. The "extra tools bypass
     the filter" doc on `BuildArgs::extra_tools` was rewritten to
     match — only call site (chat) was passing `Vec::new()`, so no
     external behavior changes.
   - `ResolvedArgs::as_build_args` gained a `&RunnerCtx` parameter
     so it can build fresh tool instances on every turn (rebuilt
     during `sdk_refresh_agent` too — the registry sender clones are
     cheap and the Tool trait isn't `Clone`, so per-call construction
     is the correct seam).
   - **`AgentSpec::transcript_snapshot` is empty for tool-spawned
     children (v1 wart).** The orchestrator persona's prompt instructs
     it to pack purpose + acceptance criteria + scope limits into the
     brief; piping the parent's transcript through every tool call
     would mean either threading the live `Agent::messages` into tool
     dispatch (no current seam) or re-loading from disk on every
     spawn (real cost for context the orchestrator isn't supposed to
     lean on). Documented in `tools/agent.rs`. Revisit if a "fork
     this conversation" tool ever shows up.
   - Id parsing accepts either `{"id": 7}` or `{"id": "agent#7"}` —
     the LLM gets the canonical display form back from `spawn_agent`,
     and accepting the bare integer keeps friction low for retries
     across providers that re-quote integers.
   - `chat` Cargo.toml gained `async-trait = "0.1.89"` (the Tool
     impl needs it; lutin-tools already depends on the same version).
   - All 15 existing tests still green; no test additions this step
     (the tools are thin shims around already-tested registry
     commands — covering them needs the end-to-end harness from
     step 9).
9. End-to-end test: orchestrator persona spawns a child, child runs to
   completion, agent-response message arrives, parent auto-takes a turn
   with the result visible.

### Success criteria

- `spawn_agent` returns immediately; parent continues chatting.
- Live agents appear in parent's system prompt every turn without explicit
  polling.
- On child completion, an agent-response message lands in the parent's
  transcript and the parent auto-takes a turn with that context.
- `Stop` aborts cleanly; no late updates resurrect stopped slots.
- Engine shutdown reaps all live agent tasks (registry drop → actor exits →
  abort handles fire).
- Recursion works: an orchestrator child can itself spawn a grandchild;
  results bubble up one level at a time.
- Zero `Arc<Mutex>` / `Arc<RwLock>` introduced.

---

## Phase 2 — Cross-workflow spawn (deferred)

Workflows are subprocesses or Docker containers; "in-process" is not on the
table. Communication has to go through the control panel (which already
abstracts over both modes for user-facing sessions).

### Why phase 2 is genuinely different from phase 1

- Process boundary → can't share `Arc<Vec<Message>>`. Args go in via the
  child workflow's typed init message, period.
- Output is not a single final value — it's the workflow's existing event
  stream over its protocol. Some kind of buffered/cursored output read on
  the parent side is unavoidable.
- Lifetime/reaping is the CP's job, not in-process abort.

So phase 2 will have its own registry shape (event buffer, cursor, demuxer)
and the tool naming is open. The two phases share *no* runtime code; only
naming conventions and the system-prompt-list trick are reusable.

### Sketch (to be detailed when we get to it)

- Control-protocol additions in `lutin-control-protocol`:
  `SpawnChildWorkflow`, `ToChild`, `StopChild`, `ListChildren`,
  events `ChildSpawned` / `ChildEvent` / `ChildExited`.
- CP-side child registry keyed by `(parent_session, task_id)`.
- Headless-mode contract in `lutin-workflow-sdk` so children don't claim UI.
- Parent-side bridge in `lutin-workflow-sdk` that demuxes `ChildEvent` by
  task id into per-child buffers.
- LLM-facing tools: design after phase 1 is in use; decide unify-vs-separate
  based on what the LLM actually needs.

### Lifetime rules

- Parent dies → CP reaps all children of that session.
- Child finishes / crashes → `ChildExited` delivered to parent.
- Docker mode: `--rm`-equivalent; CP enforces no leaked containers.

---

## Build order across both phases

1. **Phase 1 end-to-end** (sub-order above). Land it, use it.
2. CP control-protocol verbs + spawn/relay/reap, exercised by a hand-written
   test script (no parent-side bridge yet).
3. Headless contract + chat workflow opt-in.
4. Parent-side bridge.
5. Phase 2 LLM-facing tools — naming/shape decided based on phase 1
   experience.

## Explicitly out of scope

- Live transcript sharing into a child (snapshot only).
- Child writing to parent state of any kind.
- Child workflows with UIs surfaced inside parent UIs.
- Cross-session children.
- Persistent children that outlive their parent session.
- A `task_send_message` / "continue a finished child" tool — defer until a
  concrete need shows up.
- Unifying phase 1 and phase 2 internals — deliberately separate.
