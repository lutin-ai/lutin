# `crates/memory` — refactor plan

Self-contained plan to refactor `MemoryAgent` from the current `planner → fetcher (single-shot Python) → answerer` pipeline into a **tool-call loop** with optional planner and answerer.

## Current state (already done)

- `Memory::insert(NewEvent) -> EventId` — sync, no LLM
- `Memory::summarize(id) -> async Result<()>` — idempotent, runs event summary + chat rollup + entity rollups (cold-start when `summary IS NULL`)
- `LlmSummarizer { event, chat, entity: LlmStep }` — three independently swappable LLM endpoints behind feature `llm-summarizer`
- `MemoryAgent { planner: Option, fetcher, answerer }` exists but uses single-shot Python per sub-question (the part being replaced)
- `memory.query(sql, params=None)` Python binding with positional bind params
- Demo `examples/openrouter_demo.rs` loads `examples/fixtures/conversations.json` (9 chats, 124 events), inserts, summarizes with `buffer_unordered(8)`, then drops into a REPL using the agent

## Goal

Fetcher becomes an agent loop. Same LLM repeatedly calls a `run_query(python)` tool until it produces a plain-text reply (no tool calls). That plain-text reply is the sub-answer. Optional planner pre-step decomposes; optional answerer post-step synthesizes across sub-answers.

## API

```rust
// crates/memory/src/agent.rs
pub struct MemoryAgent {
    pub planner: Option<LlmStep>,    // pre-step decomposer
    pub fetcher: LlmStep,            // required — runs the tool-call loop
    pub answerer: Option<LlmStep>,   // post-step synthesizer
    pub max_iterations: usize,       // default 5; applies per fetcher run
}

pub struct Turn { pub role: Role, pub content: String }
pub enum Role { User, Assistant }

pub struct FetchTrace { pub python: String, pub stdout: String }

pub struct AgentReply {
    pub answer: String,
    pub sub_answers: Vec<String>,        // one per sub-question
    pub traces: Vec<Vec<FetchTrace>>,    // outer = sub-questions, inner = tool calls
}

#[derive(Debug)]
pub enum AgentEvent {
    PlannedQueries(Vec<String>),
    SubQuestionStart { index: usize, total: usize, intent: String },
    Tool(FetchTrace),
    SubQuestionEnd { index: usize, text: String },
    Answer(String),
}

impl MemoryAgent {
    pub async fn ask(&self, memory: Arc<Memory>, context: &[Turn], question: &str)
        -> Result<AgentReply, AgentError>;

    pub async fn ask_streaming(&self, memory: Arc<Memory>, context: &[Turn], question: &str,
        tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>)
        -> Result<AgentReply, AgentError>;
}
```

## Flow

1. **Plan**
   - `planner.is_some()` → `sub_questions = planner(context, question)` (existing JSON-output planner; keep as-is).
   - else → `sub_questions = vec![question]`.
   - Emit `PlannedQueries`.
2. **Fetch (per sub-question)**
   - Build messages:
     - `System(catalog + schema + hints)` — see below.
     - If planner is `None`: append REPL context as `User`/`Assistant` turns. If planner ran, skip context here — planner already absorbed it.
     - `User(sub_question)`.
   - Loop up to `max_iterations`:
     - `provider.complete(req with tools=[run_query])` (response_format = None — we want tool_calls or free text, not JSON).
     - Append `Message::Assistant { text, tool_calls, thinking: None }` to messages.
     - If `tool_calls.is_empty()`: this `text` is the sub-answer. Break.
     - Else for each `ToolCall`:
       - Extract `python` from `arguments` (JSON object `{"python": "..."}`).
       - Run via `tokio::task::spawn_blocking({ memory.run_python(&py) })`. Failures: capture error string into stdout, set `is_error=true` on `ToolResult` so the model can self-correct.
       - Append `Message::ToolResult(ToolResultContent { call_id, content: stdout, is_error })`.
       - Emit `Tool(FetchTrace)`.
   - If cap hit with no plain-text reply: one final `complete` with `tools = []` to force a synthesis. Use that text as sub-answer.
   - Emit `SubQuestionEnd { index, text }`.
3. **Answer**
   - `answerer.is_some()` → `answer = answerer(original_question, [(sub_q, sub_answer, traces)…])`. Use the existing answerer system prompt; pass evidence formatted as `--- query N (sub_q) ---\n<sub_answer>\n<concatenated tool stdouts>`.
   - else if `sub_answers.len() == 1`: `answer = sub_answers[0]`.
   - else: concat `sub_answers` with `## <sub_q>\n<sub_answer>` headers.
   - Emit `Answer(answer)`.

## Tool definition

```rust
ToolDefinition {
    name: ToolName::from("run_query"),
    description: "Execute a Python snippet against the memory store. The script can call \
        memory.query(sql, params=None) and memory.get(id). Anything printed to stdout is \
        returned as the tool result. Use this to gather evidence; you may call it multiple \
        times. When you have enough info to answer, reply with plain text instead of \
        another tool call.".into(),
    parameters: vec![ToolParameter {
        name: "python".into(),
        r#type: "string".into(),
        description: "Python source. Multi-line OK. Use print() for any data you want returned.".into(),
        required: true,
    }],
}
```

Tool argument extraction must handle both:
- `arguments: serde_json::Value::Object({"python": String(...)})` — common case
- `arguments: serde_json::Value::String("{\"python\":\"...\"}")` — some providers stringify

Code:
```rust
fn extract_python(args: &serde_json::Value) -> Result<String, AgentError> {
    if let Some(s) = args.get("python").and_then(|v| v.as_str()) {
        return Ok(s.to_string());
    }
    if let Some(raw) = args.as_str() {
        let v: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| AgentError::Parse(format!("tool args str: {e}")))?;
        if let Some(s) = v.get("python").and_then(|x| x.as_str()) {
            return Ok(s.to_string());
        }
    }
    Err(AgentError::Parse(format!("missing 'python' in tool args: {args}")))
}
```

## Catalog + hints (system prompt for fetcher)

Built once per `ask()` call from a single SQL pass:

```rust
fn build_catalog(memory: &Memory) -> Result<String, Error> {
    let counts = memory.query_sql(
        "SELECT 'events' AS t, count(*) AS n FROM events
         UNION ALL SELECT 'chats', count(*) FROM chats
         UNION ALL SELECT 'topics', count(*) FROM topics
         UNION ALL SELECT 'entities', count(*) FROM entities"
    )?;
    // format into the template below
}
```

Template:

```
TABLES (live counts):
  events (n={N}): id, timestamp(ms), event_type, source, content, summary, status, chat_id
    event_type ∈ {user_message, agent_message, transcription, tool_call, tool_result, note}
    status ∈ {pending, ready, failed}
  chats (n={N}): id, external_id, title, summary, started_at, last_event_at, events_since_summary, status
  topics (n={N}); event_topics(event_id, topic_id)
  entities (n={N}): id, name, kind, summary, mentions_since_summary, status
  event_entities(event_id, entity_id)
  VIEW chat_messages — events filtered to user_message + agent_message
  FTS5 events_fts(content, summary) — JOIN via events_fts.rowid = events.id
INDEXES on events: (timestamp), (event_type), (status), (chat_id)

PYTHON API (in tool):
  memory.query(sql, params=None) -> list[dict]
    # params = optional list/tuple of bind values for `?` placeholders
    # Prefer parameter binding over f-string interpolation for user-supplied terms.
  memory.get(id) -> dict | None  # full event with topics + entities

HINTS
  • Arc questions ("did we fix X", "what did we decide about Y") — start with `SELECT external_id, title, summary FROM chats`. Chat summaries are dense.
  • FTS: `events_fts MATCH 'foo OR bar OR baz'` with broad vocabulary. Avoid 3+ word phrases — they require all words present in one event.
  • Substring search: `summary LIKE '%term%'` on chats/entities, or `lower(content) LIKE lower('%term%')` on events.
  • Bind user-supplied terms: `memory.query(sql, [term])`.
  • LIMIT ≤30 on broad scans. Print only what's needed.
  • Iterate: if a query returns nothing, broaden vocabulary or switch tables. You may call run_query as many times as needed.
  • When you have enough information, reply with plain text — no more tool calls.
```

## Code locations

- `crates/memory/src/agent.rs` — replace `fetch_python` and the per-intent fetch path with the loop. Keep `plan` (planner step) and `answer` (answerer step). Make planner+answerer both `Option<LlmStep>`. Add `max_iterations`.
- `crates/memory/src/agent.rs` — add `AgentEvent` enum + `ask_streaming`. Refactor `ask` to call `ask_streaming` with a discarded receiver.
- `crates/memory/src/agent.rs` — add `build_catalog` helper.
- `crates/memory/examples/openrouter_demo.rs` — switch REPL to `ask_streaming`, spawn a printer task on the receiver, print `Tool` traces live as they arrive. Construct `MemoryAgent` with `planner: None, answerer: None, max_iterations: 5` for the simplest path; add commented-out alternatives.

## What to delete

- `MemoryAgent::fetch_python` (single-shot fetcher).
- `FETCHER_SYSTEM` constant (replaced by `build_catalog` + hint block).
- `PyReply` deserialize struct (no longer parsing `{"python": ...}` from a JSON-mode response — Python now arrives via tool args).

## Testing

1. `cargo build --example openrouter_demo` clean.
2. Run demo. Manual REPL questions to verify:
   - "Did we fix the OpenRouter streaming bug?" → must answer YES with the `.map → drain loop` fix in `crates/llm/src/openrouter.rs`. (Previously failed: FTS phrase match returned empty.)
   - "Which Lutin features did we discuss?" → expect cross-chat memory, voice-first capture, scheduled workflows, shared personas, browser extension (from chat-brainstorm-features).
   - "What did Alice work on?" → multi-chat synthesis covering Q4 launch planning + auth refactor.
   - "Has the auth migration been completed?" → should reflect status across chat-pr-review-auth and chat-refactor-auth-session.
3. Watch live trace stream — should see multiple `run_query` calls per question (typical: 2–4 per question), broadening when one returns empty.

## Risks / gotchas

- **Provider tool-call shape differences.** OpenRouter passes through OpenAI-format tool calls; the `llm` crate already normalizes via `openai_compat`. Validate by inspecting `resp.tool_calls` on the first round — should be `Vec<ToolCall>` with non-empty `arguments`.
- **`response_format` interaction with tools.** When tools are present, do NOT set `response_format = JsonObject` — it conflicts. Set to `None`.
- **DeepSeek-specific.** Some DeepSeek variants on OpenRouter only emit tool calls when `tool_choice="auto"` is explicit. The `llm` crate currently doesn't expose `tool_choice`; default behavior is auto. If empty tool_calls are returned despite obvious need, that's the next thing to debug.
- **Python failures should not abort the loop.** Wrap `memory.run_python` in a result; on error, set `is_error=true` and return the error message as the tool result content. The model can then correct its query.
- **Cap behavior.** If `max_iterations` is hit and the last response had tool_calls (no plain-text reply yet), do one more call without tools to force synthesis. Don't return the cap-failed state to the user.
- **Streaming channel.** Use `tokio::sync::mpsc::unbounded_channel`. Demo: spawn a `tokio::spawn` consuming the receiver and printing each event. Drop the sender when `ask_streaming` returns so the consumer task exits.

## Out of scope (do not do)

- Vector search.
- Failed-event retry pass at startup.
- REPL persistence.
- Engine integration (the engine wiring will happen in a separate task).
