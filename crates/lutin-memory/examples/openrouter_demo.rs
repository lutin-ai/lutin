//! End-to-end demo + interactive REPL backed by `memory::agent::MemoryAgent`.
//!
//! Run:
//!   OPENROUTER_API_KEY=sk-... \
//!   OPENROUTER_MODEL=deepseek/deepseek-chat \
//!     cargo run --release --example openrouter_demo -p memory

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};
use lutin_llm::ids::ModelId;
use lutin_llm::openrouter::{OpenRouterConfig, OpenRouterProvider};
use lutin_llm::LlmProvider;
use lutin_memory::agent::{AgentEvent, MemoryAgent, Role, Turn};
use lutin_memory::llm_summarizer::{LlmStep, LlmSummarizer};
use lutin_memory::{Config, EventType, Memory, NewEvent};
use reqwest::Client;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    chats: Vec<FixtureChat>,
}
#[derive(Deserialize)]
struct FixtureChat {
    external_id: String,
    events: Vec<FixtureEvent>,
}
#[derive(Deserialize)]
struct FixtureEvent {
    event_type: String,
    source: Option<String>,
    content: String,
    #[serde(default)]
    delta_ms: i64,
}

const SUMMARIZE_CONCURRENCY: usize = 8;
const REPL_CONTEXT_TURNS: usize = 8;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENROUTER_API_KEY").expect("set OPENROUTER_API_KEY");
    let model_str =
        std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| "deepseek/deepseek-chat".into());
    let model: ModelId = model_str.as_str().into();

    let provider: Arc<dyn LlmProvider> = Arc::new(OpenRouterProvider::new(
        OpenRouterConfig {
            api_key,
            app_name: Some("memory-demo".into()),
            app_url: None,
        },
        Client::new(),
    ));

    let summarizer = Arc::new(LlmSummarizer {
        event: LlmStep::new(provider.clone(), model.clone()),
        chat: LlmStep::new(provider.clone(), model.clone()),
        entity: LlmStep::new(provider.clone(), model.clone()),
    });

    let config = Config {
        summary_every_n_events: 8,
        summary_every_n_mentions: 4,
    };
    let memory = Arc::new(Memory::open_in_memory(config, summarizer)?);

    // ── load fixture ────────────────────────────────────────────────────
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/fixtures/conversations.json");
    let raw = std::fs::read_to_string(&fixture_path)
        .map_err(|e| format!("read {}: {e}", fixture_path.display()))?;
    let fixture: Fixture = serde_json::from_str(&raw)?;
    let total_events: usize = fixture.chats.iter().map(|c| c.events.len()).sum();
    println!(
        "loaded {} chats, {} events from {}",
        fixture.chats.len(),
        total_events,
        fixture_path.display()
    );

    // ── insert all events (sync, fast) ──────────────────────────────────
    let now_ms = chrono::Utc::now().timestamp_millis();
    let total_duration_ms: i64 = fixture
        .chats
        .iter()
        .map(|c| c.events.iter().map(|e| e.delta_ms).sum::<i64>())
        .sum();
    let mut t = now_ms - total_duration_ms;
    let mut ids: Vec<i64> = Vec::with_capacity(total_events);
    let insert_start = Instant::now();
    for chat in &fixture.chats {
        for ev in &chat.events {
            t += ev.delta_ms.max(0);
            let ty = match ev.event_type.as_str() {
                "user_message" => EventType::UserMessage,
                "agent_message" => EventType::AgentMessage,
                "transcription" => EventType::Transcription,
                "tool_call" => EventType::ToolCall,
                "tool_result" => EventType::ToolResult,
                "note" => EventType::Note,
                other => {
                    eprintln!("unknown event_type {other:?}, defaulting to note");
                    EventType::Note
                }
            };
            let id = memory.insert(NewEvent {
                timestamp: t,
                event_type: ty,
                source: ev.source.clone(),
                content: ev.content.clone(),
                chat_external_id: Some(chat.external_id.clone()),
            })?;
            ids.push(id);
        }
    }
    println!(
        "inserted {} events in {:?}",
        total_events,
        insert_start.elapsed()
    );

    // ── summarize concurrently ──────────────────────────────────────────
    let summarize_start = Instant::now();
    println!(
        "summarizing with concurrency={SUMMARIZE_CONCURRENCY}...",
    );
    let total = ids.len();
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mem = memory.clone();
    let done_c = done.clone();
    stream::iter(ids)
        .map(move |id| {
            let mem = mem.clone();
            let done = done_c.clone();
            async move {
                if let Err(e) = mem.summarize(id).await {
                    eprintln!("summarize {id} failed: {e}");
                }
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 10 == 0 || n == total {
                    println!("  [{:>5}s] summarized {n}/{total}", summarize_start.elapsed().as_secs());
                }
            }
        })
        .buffer_unordered(SUMMARIZE_CONCURRENCY)
        .for_each(|_| async {})
        .await;
    println!("summarization done in {:?}", summarize_start.elapsed());

    print_stats(&memory)?;

    // ── REPL ────────────────────────────────────────────────────────────
    // Simplest config: tool-call loop only, no planner / answerer.
    let agent = MemoryAgent {
        planner: None,
        fetcher: LlmStep::new(provider.clone(), model.clone()),
        answerer: None,
        max_iterations: 10,
        request_timeout: std::time::Duration::from_secs(300),
        max_tool_output_bytes: 8192,
    };
    // Alternative — with planner + answerer:
    // let agent = MemoryAgent {
    //     planner: Some(LlmStep::new(provider.clone(), model.clone())),
    //     fetcher: LlmStep::new(provider.clone(), model.clone()),
    //     answerer: Some(LlmStep::new(provider.clone(), model.clone())),
    //     max_iterations: 10,
    //     request_timeout: std::time::Duration::from_secs(300),
    //     max_tool_output_bytes: 8192,
    // };
    let mut history: VecDeque<Turn> = VecDeque::with_capacity(REPL_CONTEXT_TURNS);

    println!("\nask questions (blank line to exit). last {REPL_CONTEXT_TURNS} turns are kept as context.");
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    loop {
        print!("\n> ");
        stdout.flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let q = line.trim();
        if q.is_empty() {
            break;
        }

        let context: Vec<Turn> = history.iter().cloned().collect();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let printer = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentEvent::PlannedQueries(qs) => {
                        if qs.len() > 1 {
                            println!("\nplanned {} sub-questions:", qs.len());
                            for (i, q) in qs.iter().enumerate() {
                                println!("  {}. {q}", i + 1);
                            }
                        }
                    }
                    AgentEvent::SubQuestionStart { index, total, intent } => {
                        if total > 1 {
                            println!("\n=== sub-question {}/{}: {intent} ===", index + 1, total);
                        }
                    }
                    AgentEvent::Tool(t) => {
                        println!("\n--- run_query ---");
                        println!("{}", t.python.trim());
                        println!("--- stdout ---\n{}", t.stdout.trim_end());
                    }
                    AgentEvent::SubQuestionEnd { .. } => {}
                    AgentEvent::Answer(_) => {}
                }
            }
        });
        let result = agent.ask_streaming(memory.clone(), &context, q, tx).await;
        printer.await.ok();
        match result {
            Ok(reply) => {
                println!("\n{}", reply.answer);
                push_capped(&mut history, Turn { role: Role::User, content: q.to_string() });
                push_capped(&mut history, Turn { role: Role::Assistant, content: reply.answer });
            }
            Err(e) => println!("\nerror: {e}"),
        }
    }
    Ok(())
}

fn push_capped(buf: &mut VecDeque<Turn>, turn: Turn) {
    while buf.len() >= REPL_CONTEXT_TURNS {
        buf.pop_front();
    }
    buf.push_back(turn);
}

fn print_stats(memory: &Memory) -> Result<(), Box<dyn std::error::Error>> {
    let counts = memory.query_sql(
        "SELECT event_type, count(*) AS n FROM events GROUP BY event_type ORDER BY n DESC",
    )?;
    println!("\nevent_type counts:");
    for r in counts {
        println!("  {:<14} {}", r.get("event_type").unwrap(), r.get("n").unwrap());
    }
    let topics = memory.query_sql(
        "SELECT t.name, count(*) AS n FROM topics t JOIN event_topics et ON et.topic_id=t.id \
         GROUP BY t.id ORDER BY n DESC LIMIT 10",
    )?;
    println!("\ntop topics:");
    for r in topics {
        println!("  {:<24} {}", r.get("name").unwrap(), r.get("n").unwrap());
    }
    let ents = memory.query_sql(
        "SELECT name, kind, length(coalesce(summary,'')) AS slen FROM entities \
         ORDER BY slen DESC LIMIT 10",
    )?;
    println!("\nsample entities:");
    for r in ents {
        println!(
            "  {} ({:?}) summary_len={}",
            r.get("name").unwrap(),
            r.get("kind").unwrap(),
            r.get("slen").unwrap()
        );
    }
    let chats = memory.query_sql(
        "SELECT external_id, title, summary FROM chats",
    )?;
    println!("\nchats:");
    for r in chats {
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("(none)");
        let summary = r.get("summary").and_then(|v| v.as_str()).unwrap_or("(none)");
        println!(
            "  {}: {}\n    {}",
            r.get("external_id").and_then(|v| v.as_str()).unwrap_or("?"),
            title,
            summary
        );
    }
    Ok(())
}
