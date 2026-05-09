//! Question-answering agent over a [`Memory`] store.
//!
//! Pipeline:
//!   - **planner** (optional): decomposes context+question into focused sub-questions
//!   - **fetcher**: required tool-call loop. The model repeatedly calls `run_query(python)`
//!     against the store; once it replies with plain text (no tool calls) that text is
//!     the sub-answer.
//!   - **answerer** (optional): synthesises one natural-language reply across sub-answers.

use std::sync::Arc;
use std::time::Duration;

use lutin_llm::{
    CompletionRequest, Extensions, Message, Reasoning, ReasoningEffort, ResponseFormat,
    ToolDefinition, ToolName, ToolParameter, ToolResultContent,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task;

use crate::Memory;
use crate::llm_summarizer::LlmStep;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone)]
pub struct MemoryAgent {
    pub planner: Option<LlmStep>,
    pub fetcher: LlmStep,
    pub answerer: Option<LlmStep>,
    pub max_iterations: usize,
    pub request_timeout: Duration,
    pub max_tool_output_bytes: usize,
}

impl MemoryAgent {
    pub fn new(fetcher: LlmStep) -> Self {
        Self {
            planner: None,
            fetcher,
            answerer: None,
            max_iterations: 10,
            request_timeout: Duration::from_secs(300),
            max_tool_output_bytes: 8192,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("memory: {0}")]
    Memory(#[from] crate::Error),
    #[error("llm: {0}")]
    Llm(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("join: {0}")]
    Join(#[from] task::JoinError),
}

#[derive(Debug, Clone)]
pub struct FetchTrace {
    pub python: String,
    pub stdout: String,
}

#[derive(Debug, Clone)]
pub struct AgentReply {
    pub answer: String,
    pub sub_answers: Vec<String>,
    pub traces: Vec<Vec<FetchTrace>>,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    PlannedQueries(Vec<String>),
    SubQuestionStart {
        index: usize,
        total: usize,
        intent: String,
    },
    Tool(FetchTrace),
    SubQuestionEnd {
        index: usize,
        text: String,
    },
    Answer(String),
}

const RUN_QUERY_TOOL: &str = "run_query";

impl MemoryAgent {
    pub async fn ask(
        &self,
        memory: Arc<Memory>,
        context: &[Turn],
        question: &str,
    ) -> Result<AgentReply, AgentError> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // Drain on the side; we don't care about events here.
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        self.ask_streaming(memory, context, question, tx).await
    }

    pub async fn ask_streaming(
        &self,
        memory: Arc<Memory>,
        context: &[Turn],
        question: &str,
        tx: UnboundedSender<AgentEvent>,
    ) -> Result<AgentReply, AgentError> {
        let sub_questions = match &self.planner {
            Some(planner) => self.plan(planner, context, question).await?,
            None => vec![question.to_string()],
        };
        let _ = tx.send(AgentEvent::PlannedQueries(sub_questions.clone()));

        let catalog = build_catalog(&memory, self.max_tool_output_bytes)?;
        let planner_absorbed_context = self.planner.is_some();

        let mut sub_answers: Vec<String> = Vec::with_capacity(sub_questions.len());
        let mut traces: Vec<Vec<FetchTrace>> = Vec::with_capacity(sub_questions.len());
        let total = sub_questions.len();

        for (idx, sub_q) in sub_questions.iter().enumerate() {
            let _ = tx.send(AgentEvent::SubQuestionStart {
                index: idx,
                total,
                intent: sub_q.clone(),
            });

            let (text, sub_traces) = self
                .run_fetch_loop(
                    memory.clone(),
                    &catalog,
                    if planner_absorbed_context { &[] } else { context },
                    sub_q,
                    &tx,
                )
                .await?;

            let _ = tx.send(AgentEvent::SubQuestionEnd {
                index: idx,
                text: text.clone(),
            });
            sub_answers.push(text);
            traces.push(sub_traces);
        }

        let answer = match &self.answerer {
            Some(answerer) => {
                self.answer(answerer, context, question, &sub_questions, &sub_answers, &traces)
                    .await?
            }
            None if sub_answers.len() == 1 => sub_answers[0].clone(),
            None => sub_questions
                .iter()
                .zip(sub_answers.iter())
                .map(|(q, a)| format!("## {q}\n{a}"))
                .collect::<Vec<_>>()
                .join("\n\n"),
        };

        let _ = tx.send(AgentEvent::Answer(answer.clone()));

        Ok(AgentReply {
            answer,
            sub_answers,
            traces,
        })
    }

    async fn plan(
        &self,
        planner: &LlmStep,
        context: &[Turn],
        question: &str,
    ) -> Result<Vec<String>, AgentError> {
        let system = "You decompose a user's question about a memory store into a small set of \
            focused, self-contained sub-questions a retrieval agent can answer one at a time. \
            Use prior context to resolve pronouns and follow-ups. Prefer 1 sub-question when \
            the question is already concrete; up to 4 when it spans multiple aspects. \
            Output STRICT JSON: {\"queries\": [string, ...]}. No commentary.";
        let user = format!(
            "Context:\n{}\n\nUser question: {question}",
            render_context(context)
        );
        let req = CompletionRequest {
            model: planner.model.clone(),
            messages: vec![Message::System(system.to_string()), Message::User(user)],
            tools: Vec::new(),
            temperature: Some(0.1),
            max_tokens: Some(600),
            thinking_enabled: false,
            presence_penalty: None,
            extensions: Extensions {
                reasoning: Some(Reasoning {
                    effort: ReasoningEffort::Low,
                    max_tokens: None,
                }),
                response_format: Some(ResponseFormat::JsonObject),
                ignore_providers: Vec::new(),
            },
        };
        let timeout = self.request_timeout;
        let resp = tokio::time::timeout(timeout, planner.provider.complete(req))
            .await
            .map_err(|_| AgentError::Llm(format!("timeout after {timeout:?}")))?
            .map_err(|e| AgentError::Llm(format!("{e}")))?;
        let raw = resp.text;
        let parsed: PlanReply = serde_json::from_str(&raw)
            .map_err(|e| AgentError::Parse(format!("planner: {e}; raw={raw}")))?;
        if parsed.queries.is_empty() {
            return Ok(vec![question.to_string()]);
        }
        Ok(parsed.queries)
    }

    async fn run_fetch_loop(
        &self,
        memory: Arc<Memory>,
        catalog: &str,
        context: &[Turn],
        sub_question: &str,
        tx: &UnboundedSender<AgentEvent>,
    ) -> Result<(String, Vec<FetchTrace>), AgentError> {
        let mut messages: Vec<Message> = Vec::new();
        messages.push(Message::System(catalog.to_string()));
        for turn in context {
            match turn.role {
                Role::User => messages.push(Message::User(turn.content.clone())),
                Role::Assistant => messages.push(Message::Assistant {
                    text: turn.content.clone(),
                    tool_calls: Vec::new(),
                    thinking: None,
                }),
            }
        }
        messages.push(Message::User(sub_question.to_string()));

        let tools = vec![run_query_tool()];
        let mut traces: Vec<FetchTrace> = Vec::new();

        for _ in 0..self.max_iterations {
            let req = CompletionRequest {
                model: self.fetcher.model.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                temperature: Some(0.1),
                max_tokens: self.fetcher.max_tokens,
                thinking_enabled: false,
                presence_penalty: None,
                extensions: Extensions {
                    reasoning: Some(Reasoning {
                        effort: ReasoningEffort::Low,
                        max_tokens: None,
                    }),
                    response_format: None,
                    ignore_providers: Vec::new(),
                },
            };
            let timeout = self.request_timeout;
            let resp = tokio::time::timeout(timeout, self.fetcher.provider.complete(req))
                .await
                .map_err(|_| AgentError::Llm(format!("timeout after {timeout:?}")))?
                .map_err(|e| AgentError::Llm(format!("{e}")))?;

            if resp.tool_calls.is_empty() {
                return Ok((resp.text, traces));
            }
            let tool_calls = resp.tool_calls;
            messages.push(Message::Assistant {
                text: resp.text,
                tool_calls: tool_calls.clone(),
                thinking: None,
            });

            for call in &tool_calls {
                if call.name.as_str() != RUN_QUERY_TOOL {
                    messages.push(Message::ToolResult(ToolResultContent {
                        call_id: call.id.clone(),
                        content: format!("unknown tool: {}", call.name.as_str()),
                        is_error: true,
                    }));
                    continue;
                }
                let (content, is_error) = match extract_python(&call.arguments) {
                    Ok(python) => {
                        let mem = memory.clone();
                        let py = python.clone();
                        let result = task::spawn_blocking(move || mem.run_python(&py)).await?;
                        let (stdout, is_error) = match result {
                            Ok(s) => (s, false),
                            Err(e) => (format!("python error: {e}"), true),
                        };
                        let trace = FetchTrace {
                            python,
                            stdout: stdout.clone(),
                        };
                        let _ = tx.send(AgentEvent::Tool(trace.clone()));
                        traces.push(trace);
                        if !is_error && stdout.len() > self.max_tool_output_bytes {
                            (
                                format!(
                                    "output too large: {n} bytes (cap {cap}). Narrow the query: add LIMIT, project fewer columns, or filter rows.",
                                    n = stdout.len(),
                                    cap = self.max_tool_output_bytes
                                ),
                                true,
                            )
                        } else {
                            (stdout, is_error)
                        }
                    }
                    Err(e) => (format!("{e}"), true),
                };
                messages.push(Message::ToolResult(ToolResultContent {
                    call_id: call.id.clone(),
                    content,
                    is_error,
                }));
            }
        }

        // Cap hit with no plain-text reply: force synthesis with no tools.
        // Append an explicit user instruction — without it some models (notably
        // DeepSeek on OpenRouter) emit their native tool-call token format as
        // plain text when tools vanish mid-flow.
        messages.push(Message::User(
            "Iteration cap reached. No more tool calls are available. Reply with a final \
             plain-text answer based on the evidence already gathered."
                .to_string(),
        ));
        let req = CompletionRequest {
            model: self.fetcher.model.clone(),
            messages,
            tools: Vec::new(),
            temperature: Some(0.2),
            max_tokens: self.fetcher.max_tokens,
            thinking_enabled: false,
            presence_penalty: None,
            extensions: Extensions {
                reasoning: Some(Reasoning {
                    effort: ReasoningEffort::Low,
                    max_tokens: None,
                }),
                response_format: None,
                ignore_providers: Vec::new(),
            },
        };
        let timeout = self.request_timeout;
        let resp = tokio::time::timeout(timeout, self.fetcher.provider.complete(req))
            .await
            .map_err(|_| AgentError::Llm(format!("timeout after {timeout:?}")))?
            .map_err(|e| AgentError::Llm(format!("{e}")))?;
        Ok((resp.text, traces))
    }

    async fn answer(
        &self,
        answerer: &LlmStep,
        context: &[Turn],
        question: &str,
        sub_questions: &[String],
        sub_answers: &[String],
        traces: &[Vec<FetchTrace>],
    ) -> Result<String, AgentError> {
        let mut user = format!(
            "Context:\n{}\n\nUser question: {question}\n\nEvidence:\n",
            render_context(context)
        );
        for (i, ((sq, sa), ts)) in sub_questions
            .iter()
            .zip(sub_answers.iter())
            .zip(traces.iter())
            .enumerate()
        {
            user.push_str(&format!("--- query {} ({sq}) ---\n{sa}\n", i + 1));
            for t in ts {
                user.push_str(&t.stdout);
                if !t.stdout.ends_with('\n') {
                    user.push('\n');
                }
            }
        }

        let req = CompletionRequest {
            model: answerer.model.clone(),
            messages: vec![
                Message::System(ANSWERER_SYSTEM.into()),
                Message::User(user),
            ],
            tools: Vec::new(),
            temperature: Some(0.3),
            max_tokens: answerer.max_tokens,
            thinking_enabled: false,
            presence_penalty: None,
            extensions: Extensions {
                reasoning: Some(Reasoning {
                    effort: ReasoningEffort::Low,
                    max_tokens: None,
                }),
                response_format: None,
                ignore_providers: Vec::new(),
            },
        };
        let timeout = self.request_timeout;
        let resp = tokio::time::timeout(timeout, answerer.provider.complete(req))
            .await
            .map_err(|_| AgentError::Llm(format!("timeout after {timeout:?}")))?
            .map_err(|e| AgentError::Llm(format!("{e}")))?;
        Ok(resp.text)
    }
}

fn run_query_tool() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::from(RUN_QUERY_TOOL),
        description: "Execute a Python snippet against the memory store. The script can call \
            memory.query(sql, params=None) and memory.get(id). Anything printed to stdout is \
            returned as the tool result. Use this to gather evidence; you may call it multiple \
            times. When you have enough info to answer, reply with plain text instead of \
            another tool call."
            .into(),
        parameters: vec![ToolParameter {
            name: "python".into(),
            r#type: "string".into(),
            description:
                "Python source. Multi-line OK. Use print() for any data you want returned."
                    .into(),
            required: true,
        }],
    }
}

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
    Err(AgentError::Parse(format!(
        "missing 'python' in tool args: {args}"
    )))
}

fn build_catalog(memory: &Memory, max_output_bytes: usize) -> Result<String, crate::Error> {
    let rows = memory.query_sql(
        "SELECT
            (SELECT count(*) FROM events)   AS events,
            (SELECT count(*) FROM chats)    AS chats,
            (SELECT count(*) FROM topics)   AS topics,
            (SELECT count(*) FROM entities) AS entities",
    )?;
    let row = rows.first();
    let n = |k: &str| {
        row.and_then(|r| r.get(k))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    let (ev, ch, tp, en) = (n("events"), n("chats"), n("topics"), n("entities"));
    let mods = crate::python::available_modules().join(", ");

    Ok(format!(
        r#"You answer questions about a SQLite-backed memory store by repeatedly calling the
`run_query` tool with Python snippets. Print to stdout to surface evidence. When you have
enough information to answer, reply with plain text and no further tool calls.

TABLES (live counts):
  events (n={ev}): id, timestamp(ms), event_type, source, content, summary, status, chat_id
    event_type ∈ {{user_message, agent_message, transcription, tool_call, tool_result, note}}
    status ∈ {{pending, ready, failed}}
  chats (n={ch}): id, external_id, title, summary, started_at, last_event_at, events_since_summary, status
  topics (n={tp}); event_topics(event_id, topic_id)
  entities (n={en}): id, name, kind, summary, mentions_since_summary, status
  event_entities(event_id, entity_id)
  VIEW chat_messages — events filtered to user_message + agent_message
  FTS5 events_fts(content, summary) — JOIN via events_fts.rowid = events.id
INDEXES on events: (timestamp), (event_type), (status), (chat_id)

PYTHON API (in tool):
  memory.query(sql, params=None) -> list[dict]
    # params = optional list/tuple of bind values for `?` placeholders
    # Prefer parameter binding over f-string interpolation for user-supplied terms.
  memory.get(id) -> dict | None  # full event with topics + entities
  Preloaded modules (no need to import): {mods}.
  Each call runs in a fresh globals dict — assume no state persists between calls.

HINTS
  • Arc questions ("did we fix X", "what did we decide about Y") — start with `SELECT external_id, title, summary FROM chats`. Chat summaries are dense.
  • FTS: `events_fts MATCH 'foo OR bar OR baz'` with broad vocabulary. Avoid 3+ word phrases — they require all words present in one event.
  • Substring search: `summary LIKE '%term%'` on chats/entities, or `lower(content) LIKE lower('%term%')` on events.
  • Bind user-supplied terms: `memory.query(sql, [term])`.
  • LIMIT ≤30 on broad scans. Print only what's needed.
  • Tool output is capped at {cap} bytes — overflowing returns an error. Use LIMIT and project only the columns you need.
  • Iterate: if a query returns nothing, broaden vocabulary or switch tables. You may call run_query as many times as needed.
  • When you have enough information, reply with plain text — no more tool calls."#,
        ev = ev,
        ch = ch,
        tp = tp,
        en = en,
        mods = mods,
        cap = max_output_bytes,
    ))
}

const ANSWERER_SYSTEM: &str = "You write the final natural-language answer to a user's question \
    about their memory store, given the raw output of one or more retrieval queries. \
    Cite specific event ids, entity names, or chat titles when relevant. \
    If the evidence is empty or doesn't contain the answer, say so plainly. \
    Keep it under ~150 words.";

fn render_context(context: &[Turn]) -> String {
    if context.is_empty() {
        return "(none)".to_string();
    }
    let mut s = String::new();
    for t in context {
        let role = match t.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        s.push_str(&format!("[{role}] {}\n", t.content));
    }
    s
}

#[derive(Deserialize)]
struct PlanReply {
    queries: Vec<String>,
}
