//! The agent loop.
//!
//! Shape: normal chat — system + history + user, model picks a tool,
//! we run it, append the result, loop. The twist: every tool call is
//! gated by the principle reviewers, *before* it runs.
//!
//! Per tool-call slot:
//!   1. snapshot `baseline = messages.len()`
//!   2. ask the model; it produces an assistant message (maybe with a tool call)
//!   3. if no tool call → end of turn, return the assistant text as reply
//!   4. else run principles in order on the drafted call
//!        - all pass → truncate(baseline), re-push *only* the approved
//!          assistant message, execute the tool, append real tool result
//!        - any fix/rethink → leave the failed draft in messages and
//!          append a synthetic tool_result whose body *is* the feedback;
//!          loop back to step 2 so the model self-corrects with the
//!          critique in its context
//!
//! Why the rewind on pass: while a slot is iterating, the conversation
//! accumulates draft attempts + feedback. Once an approved call lands,
//! we strip every failed draft so the *next* slot only sees the clean
//! "I called X, here's the real output" record. The model never has to
//! re-derive intent across slots from a thicket of rejected attempts.

use std::future::Future;
use std::pin::Pin;

use anyhow::{Result, anyhow};
use futures_util::stream::{FuturesOrdered, StreamExt};
use lutin_llm::{CompletionRequest, Message, ToolCall, ToolResultContent};
use lutin_tools::{ToolCallContext, ToolResult};
use tracing::info;

use crate::reviewer::{review_principle, review_relevance};
use crate::trace::{log_messages, log_response};
use crate::types::{Agent, Principle, ReviewSubject, ReviewedCall, TurnOutcome, Verdict};
use crate::wire::{ChatEvent, ReviewVerdict};

/// Maximum tool-call slots per turn. Safety net only — the loop
/// terminates cleanly when the model stops calling tools.
const TURN_HARD_CAP: usize = 200;
/// Per-slot retry budget. If a single tool-call slot can't get past
/// the reviewers in this many attempts, the turn errors out rather
/// than spin forever.
const DRAFT_MAX_ATTEMPTS: usize = 20;
/// Sliding-window context budget. Before each tool-call slot we drop the
/// oldest messages (after the system prompt) until the running estimate
/// fits, so a long session can't grow the prompt without bound. There's
/// no compaction here — older history is simply forgotten.
const CONTEXT_TOKEN_LIMIT: usize = 80_000;

pub async fn run_turn(agent: &mut Agent, principles: &[Principle]) -> Result<TurnOutcome> {
    let stage = "turn";

    steer_pretext(agent, principles).await?;

    let mut rust_dirty: Option<String> = None;

    for step_id in 1..=TURN_HARD_CAP {
        // Trim before snapshotting `baseline` — the slot's truncate/rewind
        // logic indexes off it, so the window must settle first.
        slide_window(&mut agent.messages);
        let baseline = agent.messages.len();

        let mut approved: Option<ApprovedDraft> = None;
        for attempt in 0..DRAFT_MAX_ATTEMPTS {
            info!(stage, step_id, attempt, "main agent: request");
            log_messages(stage, &agent.messages);

            let request = CompletionRequest {
                model: agent.model.clone(),
                messages: agent.messages.clone(),
                tools: agent.toolbox.definitions(),
                temperature: agent.temperature,
                presence_penalty: agent.presence_penalty,
                max_tokens: None,
                thinking_enabled: agent.thinking_enabled,
                extensions: lutin_llm::Extensions {
                    reasoning: agent.reasoning.clone(),
                    ..Default::default()
                },
            };
            let response = agent.provider.complete(request).await?;
            log_response(stage, attempt, &response);

            // Surface the model's reasoning for this attempt (final reply or
            // tool-call draft alike). Live-only — not part of the persisted
            // transcript, like drafts and skips.
            if let Some(thinking) = response.thinking.as_deref() {
                if !thinking.trim().is_empty() {
                    emit(
                        agent,
                        ChatEvent::Thinking {
                            step_id: step_id as u64,
                            attempt: attempt as u32,
                            text: thinking.to_string(),
                        },
                    );
                }
            }

            let tool_calls: Vec<ToolCall> = response.tool_calls.into_iter().take(1).collect();
            agent.messages.push(Message::Assistant {
                text: response.text.clone(),
                tool_calls: tool_calls.clone(),
                thinking: response.thinking.clone(),
                thinking_signature: response.thinking_signature.clone(),
            });

            let Some(call) = tool_calls.first().cloned() else {
                // No tool call: a candidate final reply. Gate it (post-text)
                // before it reaches the user — pass yields, fail injects the
                // critique and loops so the model revises the prose.
                let reply = response.text.clone();

                // Yield gate: never finish a turn with a broken build. Runs
                // before the (more expensive) reviewers; deterministic.
                if let Some(path) = rust_dirty.as_deref() {
                    match crate::checks::cargo_check(&agent.sandbox_root, path).await {
                        Some(diag) => {
                            agent.messages.push(Message::User(format!(
                                "[build gate] cargo check fails — fix the build before \
                                finishing:\n{diag}"
                            )));
                            continue;
                        }
                        None => rust_dirty = None,
                    }
                }

                let history_snapshot: Vec<Message> = agent.messages[..baseline].to_vec();
                let (verdict, offender) = run_reviewers(
                    agent,
                    principles,
                    ReviewSubject::AgentReply(&reply),
                    &history_snapshot,
                    step_id,
                    attempt,
                )
                .await?;
                match verdict {
                    Verdict::Pass => {
                        // Strip any rejected reply drafts + critiques; keep
                        // only the clean final message in history.
                        agent.messages.truncate(baseline);
                        agent.messages.push(Message::Assistant {
                            text: reply.clone(),
                            tool_calls: Vec::new(),
                            thinking: None,
                            thinking_signature: None,
                        });
                        emit(
                            agent,
                            ChatEvent::AssistantMessage {
                                id: format!("a-{step_id}"),
                                text: reply.clone(),
                            },
                        );
                        crate::memory::record(
                            agent.memory.as_ref(),
                            &agent.chat_id,
                            lutin_memory::EventType::AgentMessage,
                            reply.clone(),
                        );
                        return Ok(TurnOutcome::Yield { reply });
                    }
                    Verdict::Fail(feedback) => {
                        let body = match offender {
                            Some(name) => format!("[{name}] {feedback}"),
                            None => feedback,
                        };
                        agent.messages.push(Message::User(body));
                        continue;
                    }
                }
            };

            let drafted = ReviewedCall {
                tool: call.name.as_str().to_string(),
                goal: response.text.clone(),
                args: call.arguments.clone(),
            };
            emit(
                agent,
                ChatEvent::ToolCallDrafted {
                    step_id: step_id as u64,
                    attempt: attempt as u32,
                    tool: drafted.tool.clone(),
                    args: serde_json::to_string(&drafted.args).unwrap_or_else(|_| "{}".into()),
                },
            );

            if let Some(rejection) = oversized_edit(&call) {
                agent.messages.push(Message::ToolResult(ToolResultContent {
                    call_id: call.id.clone(),
                    content: rejection,
                    is_error: true,
                }));
                continue;
            }

            let history_snapshot: Vec<Message> = agent.messages[..baseline].to_vec();
            let (verdict, offender) = run_reviewers(
                agent,
                principles,
                ReviewSubject::Tool(&drafted),
                &history_snapshot,
                step_id,
                attempt,
            )
            .await?;

            match verdict {
                Verdict::Pass => {
                    approved = Some(ApprovedDraft {
                        assistant: agent.messages.last().cloned().expect("just pushed"),
                        call,
                    });
                    break;
                }
                Verdict::Fail(feedback) => {
                    let body = match offender {
                        Some(name) => format!(
                            "TOOL CALL REJECTED by principle reviewer [{name}] — the tool was NOT executed. Revise and try again. Feedback: {feedback}"
                        ),
                        None => format!(
                            "TOOL CALL REJECTED by principle review — the tool was NOT executed. Revise and try again. Feedback: {feedback}"
                        ),
                    };
                    // Feedback is injected as the synthetic "tool result"
                    // so the model treats it as the consequence of its
                    // drafted call.
                    agent.messages.push(Message::ToolResult(ToolResultContent {
                        call_id: call.id.clone(),
                        content: body,
                        is_error: true,
                    }));
                    // Loop back: model will see its bad draft + the
                    // feedback and produce a corrected call.
                }
            }
        }

        let Some(ApprovedDraft { assistant, call }) = approved else {
            return Err(anyhow!(
                "review slot exhausted after {DRAFT_MAX_ATTEMPTS} attempts (no approved call or reply)"
            ));
        };

        // Rewind every draft+feedback exchange and replace with just the
        // approved assistant message + the real tool result. Thinking (and
        // its signature) is carried over — Anthropic requires signed
        // thinking blocks to accompany tool calls when thinking is on.
        let (thinking, thinking_signature) = match &assistant {
            Message::Assistant {
                thinking,
                thinking_signature,
                ..
            } => (thinking.clone(), thinking_signature.clone()),
            _ => (None, None),
        };
        agent.messages.truncate(baseline);
        agent.messages.push(Message::Assistant {
            text: String::new(),
            tool_calls: vec![call.clone()],
            thinking,
            thinking_signature,
        });

        let args_str = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into());
        let ctx = ToolCallContext::default();
        let result = agent.toolbox.call(&ctx, call.clone()).await;
        let (mut content, is_error) = match result {
            ToolResult::Ok(rc) => (rc.content, false),
            ToolResult::Err(e) => (format!("[tool error] {e}"), true),
            _ => ("[tool error] unknown ToolResult variant".into(), true),
        };

        // Post-edit probes: format the file (so the working-set refresh
        // re-reads the formatted state), then an advisory cargo check —
        // intermediate breakage is allowed; the yield gate enforces green.
        if !is_error && crate::working_set::is_edit_tool(call.name.as_str()) {
            if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                if crate::checks::is_rust_file(path) {
                    rust_dirty = Some(path.to_string());
                    if let Some(fmt_err) =
                        crate::checks::format_file(&agent.sandbox_root, path).await
                    {
                        content.push_str(&format!(
                            "\n\n[rustfmt] failed — the file likely has syntax errors:\n{fmt_err}"
                        ));
                    }
                    match crate::checks::cargo_check(&agent.sandbox_root, path).await {
                        None => content.push_str("\n\n[cargo check] pass"),
                        Some(diag) => content.push_str(&format!(
                            "\n\n[cargo check] FAILED (advisory — fine mid-change, but the \
                            build must pass before you finish the turn):\n{diag}"
                        )),
                    }
                }
            }
        }

        agent.messages.push(Message::ToolResult(ToolResultContent {
            call_id: call.id.clone(),
            content: content.clone(),
            is_error,
        }));

        // Memory tool lookups aren't recorded — they'd accumulate the
        // agent's own queries as events.
        if !crate::memory::is_memory_tool(call.name.as_str()) {
            crate::memory::record(
                agent.memory.as_ref(),
                &agent.chat_id,
                lutin_memory::EventType::ToolCall,
                format!("{}({args_str})", call.name.as_str()),
            );
            crate::memory::record(
                agent.memory.as_ref(),
                &agent.chat_id,
                lutin_memory::EventType::ToolResult,
                content.clone(),
            );
        }

        emit(
            agent,
            ChatEvent::ToolCallExecuted {
                step_id: step_id as u64,
                tool: call.name.as_str().to_string(),
                args: args_str,
                output: content,
            },
        );

        if !is_error {
            crate::working_set::refresh_after_edit(agent, &call).await;
        }
        crate::working_set::compact(&mut agent.messages);
    }

    Err(anyhow!(
        "turn hit hard cap of {TURN_HARD_CAP} tool-call slots without yielding"
    ))
}

/// Pre-text steer: before the agent acts, run the message-level principles
/// on the incoming user request and inject any objection as advisory
/// context. Unlike the tool and post-text gates this does not block — it
/// nudges the agent's approach for the turn. Reported under step 0.
async fn steer_pretext(agent: &mut Agent, principles: &[Principle]) -> Result<()> {
    let Some(idx) = agent
        .messages
        .iter()
        .rposition(|m| matches!(m, Message::User(_)))
    else {
        return Ok(());
    };
    let Message::User(text) = agent.messages[idx].clone() else {
        return Ok(());
    };
    let history: Vec<Message> = agent.messages[..idx].to_vec();
    let (verdict, offender) =
        run_reviewers(agent, principles, ReviewSubject::UserMessage(&text), &history, 0, 0).await?;
    if let Verdict::Fail(feedback) = verdict {
        let body = match offender {
            Some(name) => format!("[principle review of this request — {name}] {feedback}"),
            None => format!("[principle review of this request] {feedback}"),
        };
        agent.messages.push(Message::User(body));
    }
    Ok(())
}

struct ApprovedDraft {
    assistant: Message,
    call: ToolCall,
}

/// Deterministic pre-review gate: bounce write/edit calls whose payload
/// exceeds the line limit without spending reviewer calls on them.
const EDIT_LINE_LIMIT: usize = 50;

fn oversized_edit(call: &ToolCall) -> Option<String> {
    let arg = match call.name.as_str() {
        "write" | "edit_lines" => "content",
        "edit" => "new_string",
        _ => return None,
    };
    let text = call.arguments.get(arg)?.as_str()?;
    let lines = text.lines().count();
    (lines > EDIT_LINE_LIMIT).then(|| {
        format!(
            "TOOL CALL REJECTED before review — the tool was NOT executed. This `{}` call \
            writes {lines} lines; the limit is {EDIT_LINE_LIMIT} lines per call. Split the \
            change into smaller pieces of at most {EDIT_LINE_LIMIT} lines each and apply \
            them one at a time.",
            call.name.as_str()
        )
    })
}

/// Drop the oldest messages (after the system prompt) until the running
/// token estimate fits `CONTEXT_TOKEN_LIMIT`. Best-effort: the newest
/// message and the current user turn are always kept even if they alone
/// blow the budget, and the kept window never starts on a message that
/// depends on a dropped predecessor (a tool result without its tool call,
/// or an image following one) — that would be a malformed request.
fn slide_window(messages: &mut Vec<Message>) {
    let sys_end = messages
        .iter()
        .take_while(|m| matches!(m, Message::System(_)))
        .count();
    if messages.len() <= sys_end {
        return;
    }

    let sys_tokens: usize = messages[..sys_end].iter().map(estimate_tokens).sum();
    let budget = CONTEXT_TOKEN_LIMIT.saturating_sub(sys_tokens);

    // Walk newest→oldest, keeping messages while they fit. The newest is
    // always kept (the loop seeds `keep_from` before checking the budget).
    let mut used = 0usize;
    let mut keep_from = messages.len();
    for i in (sys_end..messages.len()).rev() {
        let t = estimate_tokens(&messages[i]);
        if keep_from != messages.len() && used + t > budget {
            break;
        }
        used += t;
        keep_from = i;
    }

    // A tool result / image at the window's front references something we
    // just dropped — advance past it to a self-contained start.
    while keep_from < messages.len()
        && matches!(messages[keep_from], Message::ToolResult(_) | Message::Image { .. })
    {
        keep_from += 1;
    }

    // Never strand the current turn: clamp back so the last user message
    // (a clean start) and everything after it always survive.
    if let Some(last_user) = messages.iter().rposition(|m| matches!(m, Message::User(_))) {
        if last_user >= sys_end {
            keep_from = keep_from.min(last_user);
        }
    }

    if keep_from > sys_end {
        messages.drain(sys_end..keep_from);
    }
}

/// Rough token estimate (~4 chars/token). Only used to bound the prompt,
/// so a loose heuristic is fine — we trim conservatively.
fn estimate_tokens(msg: &Message) -> usize {
    let chars = match msg {
        Message::System(s) | Message::User(s) => s.chars().count(),
        Message::Summary { text } => text.chars().count(),
        Message::Assistant {
            text,
            tool_calls,
            thinking,
            ..
        } => {
            text.chars().count()
                + thinking.as_deref().map_or(0, |t| t.chars().count())
                + tool_calls
                    .iter()
                    .map(|c| c.name.as_str().chars().count() + c.arguments.to_string().chars().count())
                    .sum::<usize>()
        }
        Message::ToolResult(rc) => rc.content.chars().count(),
        // Images don't have a character length; charge a flat ~1k tokens
        // each so they still count against the budget.
        Message::Image { items } => items.len() * 4_000,
        Message::SubAgentReply { text, .. } => text.chars().count(),
        Message::SubAgentFailure { reason, .. } => reason.chars().count(),
    };
    chars / 4 + 4
}

/// Walk the principle tree against the drafted call. Each level is fired
/// concurrently but awaited in tree (priority) order: the first branch
/// that blocks (fix/rethink) is decisive — it outranks every later branch,
/// so we surface its verdict and drop the rest (which cancels them). Same
/// semantics as a serial short-circuit, latency collapsed sum→max. A
/// gate's relevance check still prunes its subtree before its children
/// fire; recency-trusted leaves are skipped before firing.
async fn run_reviewers(
    agent: &Agent,
    principles: &[Principle],
    subject: ReviewSubject<'_>,
    history: &[Message],
    step_id: usize,
    attempt: usize,
) -> Result<(Verdict, Option<String>)> {
    let tasks = crate::tasks::reviewer_block(&agent.state_dir);
    match eval_level(agent, principles, subject, history, &tasks, step_id, attempt).await? {
        Some((verdict, name)) => Ok((verdict, Some(name))),
        None => Ok((Verdict::Pass, None)),
    }
}

fn applies(principle: &Principle, tool: &str) -> bool {
    principle.applies_to.is_empty() || principle.applies_to.iter().any(|t| t == tool)
}

type EvalFut<'a> =
    Pin<Box<dyn Future<Output = Result<Option<(Verdict, String)>>> + Send + 'a>>;

fn ready_pass<'a>() -> EvalFut<'a> {
    Box::pin(async { Ok(None) })
}

/// Fire every applicable node at this level concurrently, then resolve in
/// order; the first `Some` blocks and cancels the rest. `Ok(None)` = no
/// objection from this level.
fn eval_level<'a>(
    agent: &'a Agent,
    nodes: &'a [Principle],
    subject: ReviewSubject<'a>,
    history: &'a [Message],
    tasks: &'a Option<String>,
    step_id: usize,
    attempt: usize,
) -> EvalFut<'a> {
    Box::pin(async move {
        let mut ordered = FuturesOrdered::new();
        for node in nodes {
            if !applies(node, subject.trigger()) {
                ordered.push_back(ready_pass());
            } else if node.is_gate() {
                ordered.push_back(eval_gate(agent, node, subject, history, tasks, step_id, attempt));
            } else {
                let decision = agent
                    .recency
                    .lock()
                    .expect("recency lock")
                    .decide(node, subject.recency_key());
                match decision {
                    crate::recency::Decision::Skip { streak } => {
                        emit(
                            agent,
                            ChatEvent::PrincipleSkipped {
                                step_id: step_id as u64,
                                attempt: attempt as u32,
                                principle: node.name.clone(),
                                streak,
                            },
                        );
                        ordered.push_back(ready_pass());
                    }
                    crate::recency::Decision::Check => {
                        ordered.push_back(eval_leaf(
                            agent, node, subject, history, tasks, step_id, attempt,
                        ));
                    }
                }
            }
        }
        while let Some(res) = ordered.next().await {
            if let Some(blocked) = res? {
                return Ok(Some(blocked));
            }
        }
        Ok(None)
    })
}

fn eval_gate<'a>(
    agent: &'a Agent,
    node: &'a Principle,
    subject: ReviewSubject<'a>,
    history: &'a [Message],
    tasks: &'a Option<String>,
    step_id: usize,
    attempt: usize,
) -> EvalFut<'a> {
    Box::pin(async move {
        let persona = load_persona(agent, node)?;
        let provider = provider_for(agent, &persona)?;
        let relevant = review_relevance(
            provider.as_ref(),
            &agent.model,
            &persona,
            node,
            subject,
            history,
            tasks.as_deref(),
            &agent.toolbox,
        )
        .await?;
        info!(stage = "gate", principle = %node.name, relevant, "relevance check");
        if !relevant {
            return Ok(None);
        }
        eval_level(agent, &node.children, subject, history, tasks, step_id, attempt).await
    })
}

fn eval_leaf<'a>(
    agent: &'a Agent,
    node: &'a Principle,
    subject: ReviewSubject<'a>,
    history: &'a [Message],
    tasks: &'a Option<String>,
    step_id: usize,
    attempt: usize,
) -> EvalFut<'a> {
    Box::pin(async move {
        let persona = load_persona(agent, node)?;
        let provider = provider_for(agent, &persona)?;
        let v = review_principle(
            provider.as_ref(),
            &agent.model,
            &persona,
            node,
            subject,
            history,
            tasks.as_deref(),
            &agent.toolbox,
        )
        .await?;
        emit(
            agent,
            ChatEvent::PrincipleEvaluated {
                step_id: step_id as u64,
                attempt: attempt as u32,
                principle: node.name.clone(),
                verdict: ReviewVerdict::from(&v),
            },
        );
        let passed = matches!(v, Verdict::Pass);
        agent
            .recency
            .lock()
            .expect("recency lock")
            .record(&node.name, subject.recency_key(), passed);
        if passed {
            Ok(None)
        } else {
            Ok(Some((v, node.name.clone())))
        }
    })
}

fn load_persona(agent: &Agent, node: &Principle) -> Result<lutin_entities::Persona> {
    lutin_entities::Persona::load(&agent.resolver, &node.persona).map_err(|e| {
        anyhow!("load persona `{}` for principle `{}`: {e}", node.persona, node.name)
    })
}

/// Resolve the provider a reviewer persona should run on. A persona with
/// its own `provider` gets that provider (the main agent's may point at a
/// different vendor whose model catalogue doesn't include the reviewer's
/// model); without one it rides the main agent's.
fn provider_for(
    agent: &Agent,
    persona: &lutin_entities::Persona,
) -> Result<std::sync::Arc<dyn lutin_llm::LlmProvider>> {
    let Some(name) = persona.provider.as_deref() else {
        return Ok(agent.provider.clone());
    };
    if let Some(p) = agent
        .reviewer_providers
        .lock()
        .expect("reviewer provider lock")
        .get(name)
    {
        return Ok(p.clone());
    }
    let settings = lutin_settings::Settings::load(&agent.resolver)
        .map_err(|e| anyhow!("load settings for reviewer provider `{name}`: {e}"))?;
    let cfg = settings
        .providers
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| anyhow!("reviewer provider not configured: {name}"))?;
    let provider = lutin_workflow_sdk::agent::build_provider(cfg)
        .map_err(|e| anyhow!("build reviewer provider `{name}`: {e}"))?;
    agent
        .reviewer_providers
        .lock()
        .expect("reviewer provider lock")
        .insert(name.to_string(), provider.clone());
    Ok(provider)
}

fn emit(agent: &Agent, event: ChatEvent) {
    let _ = agent.events.send(event);
}
