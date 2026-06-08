//! Episodic memory for the reviewed workflow.
//!
//! Two halves, both backed by one per-session SQLite store
//! (`memory.sqlite3` under the session state dir):
//!
//!   - **recording** — every user message, assistant reply, and executed
//!     tool call/result is written as an `Event`; a background task runs
//!     the summarize cascade so later recall has topics/entities/summaries.
//!   - **retrieval** — three tools handed to the MAIN agent so it can pull
//!     past context on demand: `recall` (FTS search), `fetch` (rehydrate
//!     one event), and `memory_query` (read-only SELECT escape hatch).
//!
//! Everything is fail-soft: if the store can't open (e.g. no provider for
//! the summarizer) memory is simply absent — tools report it, recording is
//! skipped, and a turn never breaks because of memory.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use lutin_entities::Persona;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter, ToolResultContent};
use lutin_memory::llm_summarizer::{LlmStep, LlmSummarizer};
use lutin_memory::{Config, EventType, Memory, NewEvent, Summarizer};
use lutin_settings::Settings;
use lutin_storage::Resolver;
use lutin_tools::{Tool, ToolCallContext, ToolResult};
use lutin_workflow_sdk::agent::{BuildArgs, build_inputs};
use lutin_workflow_sdk::prompt::PromptExtras;
use serde_json::json;
use tracing::warn;

const DB_FILE: &str = "memory.sqlite3";
const DEFAULT_PERSONA: &str = "coder";
const RECALL: &str = "recall";
const FETCH: &str = "fetch";
const MEMORY_QUERY: &str = "memory_query";
const MAX_OUTPUT: usize = 12_000;
const DEFAULT_RECALL_LIMIT: u64 = 8;

/// Open the per-session memory store. Builds the summarizer from the
/// default persona's provider, so it works headlessly without a turn in
/// flight. Returns `None` (fail-soft) if anything required is missing.
pub fn open_session(
    resolver: &Arc<Resolver>,
    project_config_dir: &Path,
    state_dir: &Path,
) -> Option<Arc<Memory>> {
    let persona = match Persona::load(resolver, DEFAULT_PERSONA) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "reviewed memory: load persona for summarizer failed; memory disabled");
            return None;
        }
    };
    let settings = Settings::load(resolver).ok()?;
    let sandbox_root = project_config_dir.parent()?.to_path_buf();
    let (config, _toolbox) = build_inputs(BuildArgs {
        persona: &persona,
        settings: &settings,
        sandbox_root,
        model_override: None,
        extra_tools: Vec::new(),
        prompt_extras: PromptExtras::default(),
        disable_streaming: true,
    })
    .map_err(|e| warn!(error = %e, "reviewed memory: build summarizer provider failed; memory disabled"))
    .ok()?;

    let step = LlmStep::new(config.provider.clone(), config.model.clone());
    let summarizer: Arc<dyn Summarizer> = Arc::new(LlmSummarizer::uniform(step));

    if let Err(e) = std::fs::create_dir_all(state_dir) {
        warn!(error = %e, "reviewed memory: create state dir failed; memory disabled");
        return None;
    }
    let path = state_dir.join(DB_FILE);
    match Memory::open(&path, Config::default(), summarizer) {
        Ok(m) => Some(Arc::new(m)),
        Err(e) => {
            warn!(error = %e, path = %path.display(), "reviewed memory: open failed; memory disabled");
            None
        }
    }
}

/// The retrieval tools to splice into the main agent's toolbox.
pub fn tools(memory: Arc<Memory>) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(RecallTool { memory: memory.clone() }),
        Box::new(FetchTool { memory: memory.clone() }),
        Box::new(QueryTool { memory }),
    ]
}

/// True for the three retrieval tools — their calls are not themselves
/// recorded as events (memory shouldn't accumulate its own lookups).
pub fn is_memory_tool(name: &str) -> bool {
    matches!(name, RECALL | FETCH | MEMORY_QUERY)
}

/// Record one event, then summarize it in the background. No-op when
/// memory is absent; never blocks the turn.
pub fn record(memory: Option<&Arc<Memory>>, chat_id: &str, event_type: EventType, content: String) {
    let Some(memory) = memory else { return };
    let event = NewEvent {
        timestamp: Utc::now().timestamp_millis(),
        event_type,
        source: None,
        content,
        chat_external_id: Some(chat_id.to_string()),
    };
    match memory.insert(event) {
        Ok(id) => {
            let mem = memory.clone();
            tokio::spawn(async move {
                if let Err(e) = mem.summarize(id).await {
                    warn!(error = %e, id, "reviewed memory: summarize failed");
                }
            });
        }
        Err(e) => warn!(error = %e, "reviewed memory: insert failed"),
    }
}

struct RecallTool {
    memory: Arc<Memory>,
}
struct FetchTool {
    memory: Arc<Memory>,
}
struct QueryTool {
    memory: Arc<Memory>,
}

#[async_trait]
impl Tool for RecallTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(RECALL),
            description: "Search long-term memory (past messages and tool results from this and \
                prior sessions) by keywords. Returns ranked hits, each with an id and a one-line \
                summary; call fetch(id) for the full content."
                .into(),
            parameters: vec![
                ToolParameter {
                    name: "query".into(),
                    r#type: "string".into(),
                    description: "Keywords to search for.".into(),
                    required: true,
                },
                ToolParameter {
                    name: "limit".into(),
                    r#type: "integer".into(),
                    description: "Max hits (default 8, max 25).".into(),
                    required: false,
                },
            ],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let query = call.arguments.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = call
            .arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_RECALL_LIMIT)
            .clamp(1, 25);
        ok(call.id, do_recall(&self.memory, query, limit))
    }
}

#[async_trait]
impl Tool for FetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(FETCH),
            description: "Rehydrate the full stored content of one memory event by its id (from a \
                recall() hit)."
                .into(),
            parameters: vec![ToolParameter {
                name: "id".into(),
                r#type: "integer".into(),
                description: "Event id, e.g. from a recall() hit.".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let Some(id) = call.arguments.get("id").and_then(|v| v.as_i64()) else {
            return err(call.id, "fetch: missing integer 'id'".into());
        };
        ok(call.id, do_fetch(&self.memory, id))
    }
}

#[async_trait]
impl Tool for QueryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(MEMORY_QUERY),
            description: "Run a read-only SELECT against the memory database. Tables: events(id, \
                timestamp, event_type, source, content, summary, status, chat_id), chats(id, \
                external_id, title, summary), topics(id, name), entities(id, name, kind, summary), \
                event_topics(event_id, topic_id), event_entities(event_id, entity_id); FTS5 \
                events_fts(content, summary) joined via events_fts.rowid = events.id. Only a single \
                SELECT statement is allowed."
                .into(),
            parameters: vec![ToolParameter {
                name: "sql".into(),
                r#type: "string".into(),
                description: "A single read-only SELECT statement.".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let sql = call.arguments.get("sql").and_then(|v| v.as_str()).unwrap_or("");
        ok(call.id, do_query(&self.memory, sql))
    }
}

fn do_recall(memory: &Memory, query: &str, limit: u64) -> String {
    let terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if terms.is_empty() {
        return format!("No memory matches for \"{query}\".");
    }
    let match_expr = terms.join(" OR ");
    let sql = "SELECT e.id AS id, e.event_type AS event_type, \
        coalesce(e.summary, substr(e.content, 1, 200)) AS summary \
        FROM events_fts f JOIN events e ON e.id = f.rowid \
        WHERE events_fts MATCH ?1 ORDER BY rank LIMIT ?2";
    let rows = match memory
        .store()
        .query_sql_with_params(sql, &[json!(match_expr), json!(limit)])
    {
        Ok(r) => r,
        Err(e) => return format!("recall error: {e}"),
    };
    if rows.is_empty() {
        return format!("No memory matches for \"{query}\".");
    }
    let mut out = format!(
        "{} match(es) for \"{query}\" (fetch(id) for full content):\n\n",
        rows.len()
    );
    for r in &rows {
        let id = r.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let ty = r.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
        let summary = r.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("[#{id}] {ty}\n  {summary}\n"));
    }
    clip(out)
}

fn do_fetch(memory: &Memory, id: i64) -> String {
    match memory.get(id) {
        Ok(Some(ev)) => {
            let topics = if ev.topics.is_empty() {
                String::new()
            } else {
                format!("\ntopics: {}", ev.topics.join(", "))
            };
            let entities = if ev.entities.is_empty() {
                String::new()
            } else {
                let names: Vec<&str> = ev.entities.iter().map(|e| e.name.as_str()).collect();
                format!("\nentities: {}", names.join(", "))
            };
            clip(format!(
                "event #{} · {}{topics}{entities}\n---\n{}",
                ev.id,
                ev.event_type.as_str(),
                ev.content
            ))
        }
        Ok(None) => format!("No event with id {id}."),
        Err(e) => format!("fetch error: {e}"),
    }
}

fn do_query(memory: &Memory, sql: &str) -> String {
    if let Err(e) = assert_read_only_select(sql) {
        return format!("query error: {e}");
    }
    match memory.query_sql(sql) {
        Ok(rows) => clip(serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into())),
        Err(e) => format!("query error: {e}"),
    }
}

/// Enforce a single bare `SELECT` — the store itself has no guard, so the
/// escape-hatch tool must never let the model mutate the database.
fn assert_read_only_select(sql: &str) -> Result<(), String> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err("empty statement".into());
    }
    if let Some(i) = trimmed.find(';') {
        if !trimmed[i + 1..].trim().is_empty() {
            return Err("only a single statement is allowed".into());
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    let select = lower.starts_with("select")
        && lower[6..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
    if !select {
        return Err("only read-only SELECT statements are allowed".into());
    }
    Ok(())
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_OUTPUT {
        s.truncate(MAX_OUTPUT);
        s.push_str("\n…[truncated]");
    }
    s
}

fn ok(call_id: lutin_llm::CallId, content: String) -> ToolResult {
    ToolResult::Ok(ToolResultContent {
        call_id,
        content,
        is_error: false,
    })
}

fn err(call_id: lutin_llm::CallId, content: String) -> ToolResult {
    ToolResult::Ok(ToolResultContent {
        call_id,
        content,
        is_error: true,
    })
}
