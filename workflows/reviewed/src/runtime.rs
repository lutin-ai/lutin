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

use anyhow::{Result, anyhow};
use lutin_llm::{CompletionRequest, Message, ToolCall, ToolResultContent};
use lutin_tools::{ToolCallContext, ToolResult};
use tracing::info;

use crate::reviewer::review_principle;
use crate::trace::{log_messages, log_response};
use crate::types::{Agent, Principle, ReviewedCall, TurnOutcome, Verdict};
use crate::wire::{ChatEvent, ReviewVerdict};

/// Maximum tool-call slots per turn. Safety net only — the loop
/// terminates cleanly when the model stops calling tools.
const TURN_HARD_CAP: usize = 200;
/// Per-slot retry budget. If a single tool-call slot can't get past
/// the reviewers in this many attempts, the turn errors out rather
/// than spin forever.
const DRAFT_MAX_ATTEMPTS: usize = 20;

pub async fn run_turn(agent: &mut Agent, principles: &[Principle]) -> Result<TurnOutcome> {
    let stage = "turn";
    for step_id in 0..TURN_HARD_CAP {
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
                thinking_enabled: false,
                extensions: Default::default(),
            };
            let response = agent.provider.complete(request).await?;
            log_response(stage, attempt, &response);

            let tool_calls: Vec<ToolCall> = response.tool_calls.into_iter().take(1).collect();
            agent.messages.push(Message::Assistant {
                text: response.text.clone(),
                tool_calls: tool_calls.clone(),
                thinking: response.thinking.clone(),
            });

            let Some(call) = tool_calls.first().cloned() else {
                // No tool call. End of turn — keep the assistant message
                // (with its text) in history as the model's final reply.
                emit(
                    agent,
                    ChatEvent::AssistantMessage {
                        id: format!("a-{step_id}"),
                        text: response.text.clone(),
                    },
                );
                return Ok(TurnOutcome::Yield {
                    reply: response.text,
                });
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

            let history_snapshot: Vec<Message> = agent.messages[..baseline].to_vec();
            let (verdict, offender) = run_reviewers(
                agent,
                principles,
                &drafted,
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
                Verdict::Fix(feedback) | Verdict::Rethink(feedback) => {
                    let body = match offender {
                        Some(name) => format!("[{name}] {feedback}"),
                        None => feedback,
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
                "tool-call slot exhausted after {DRAFT_MAX_ATTEMPTS} review attempts"
            ));
        };

        // Rewind every draft+feedback exchange and replace with just the
        // approved assistant message + the real tool result.
        agent.messages.truncate(baseline);
        agent.messages.push(Message::Assistant {
            text: String::new(),
            tool_calls: vec![call.clone()],
            thinking: None,
        });
        // agent.messages.push(assistant);

        let args_str = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into());
        let ctx = ToolCallContext::default();
        let result = agent.toolbox.call(&ctx, call.clone()).await;
        let (content, is_error) = match result {
            ToolResult::Ok(rc) => (rc.content, false),
            ToolResult::Err(e) => (format!("[tool error] {e}"), true),
            _ => ("[tool error] unknown ToolResult variant".into(), true),
        };
        agent.messages.push(Message::ToolResult(ToolResultContent {
            call_id: call.id.clone(),
            content: content.clone(),
            is_error,
        }));

        emit(
            agent,
            ChatEvent::ToolCallExecuted {
                step_id: step_id as u64,
                tool: call.name.as_str().to_string(),
                args: args_str,
                output: content,
            },
        );
    }

    Err(anyhow!(
        "turn hit hard cap of {TURN_HARD_CAP} tool-call slots without yielding"
    ))
}

struct ApprovedDraft {
    assistant: Message,
    call: ToolCall,
}

async fn run_reviewers(
    agent: &Agent,
    principles: &[Principle],
    drafted: &ReviewedCall,
    history: &[Message],
    step_id: usize,
    attempt: usize,
) -> Result<(Verdict, Option<String>)> {
    for principle in principles {
        if !principle.applies_to.is_empty()
            && !principle.applies_to.iter().any(|t| t == &drafted.tool)
        {
            continue;
        }
        let persona =
            lutin_entities::Persona::load(&agent.resolver, &principle.persona).map_err(|e| {
                anyhow!(
                    "load persona `{}` for principle `{}`: {e}",
                    principle.persona,
                    principle.name
                )
            })?;
        let v = review_principle(
            agent.provider.as_ref(),
            &agent.model,
            &persona,
            principle,
            drafted,
            history,
        )
        .await?;
        emit(
            agent,
            ChatEvent::PrincipleEvaluated {
                step_id: step_id as u64,
                attempt: attempt as u32,
                principle: principle.name.clone(),
                verdict: ReviewVerdict::from(&v),
            },
        );
        if !matches!(v, Verdict::Pass) {
            return Ok((v, Some(principle.name.clone())));
        }
    }
    Ok((Verdict::Pass, None))
}

fn emit(agent: &Agent, event: ChatEvent) {
    let _ = agent.events.send(event);
}
