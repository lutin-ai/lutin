pub mod detect;
pub mod recovery;
pub mod round;

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use lutin_llm::{CompletionRequest, Message, Usage};
use tokio::sync::mpsc;

use crate::{
    approval::ApprovalPolicy,
    config::AgentConfig,
    error::AgentError,
    event::AgentEvent,
    loop_control::{
        FinishReason, LoopConfig, PreRoundOutput, RoundSummary, StopCondition, ToolCallRecord,
    },
    outcome::RunOutcome,
    sampling::SamplingParams,
    tools::Toolbox,
};

use detect::LoopDetector;
use recovery::RetryBudget;
use round::{run_round, RoundInput, RoundOutput};

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Terminal-failure helper. `cancel = true` means user-requested cancellation
/// (no `AgentEvent::Error`, terminal `FinishReason::Cancelled`); otherwise the
/// error is broadcast on the event stream and folded into
/// `FinishReason::Error(_)` so the outcome cannot claim "errored" without an
/// error or vice versa.
fn fail(
    events: &mpsc::UnboundedSender<AgentEvent>,
    finish: &mut FinishReason,
    err: AgentError,
    cancel: bool,
) {
    if cancel {
        *finish = FinishReason::Cancelled;
        return;
    }
    let ae = Arc::new(err);
    let _ = events.send(AgentEvent::Error(Arc::clone(&ae)));
    *finish = FinishReason::Error(ae);
}

pub struct RunInputs {
    pub(crate) config: AgentConfig,
    pub(crate) messages: Vec<Message>,
    pub(crate) tools: Arc<Toolbox>,
    pub(crate) approval: Arc<dyn ApprovalPolicy>,
}

pub(crate) struct DriveResult {
    pub outcome: RunOutcome,
    pub messages: Vec<Message>,
}

#[tracing::instrument(skip_all, fields(max_rounds = inputs.config.loop_config.max_rounds))]
pub async fn drive(
    inputs: RunInputs,
    events: mpsc::UnboundedSender<AgentEvent>,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> DriveResult {
    let RunInputs { config, mut messages, tools, approval } = inputs;

    let AgentConfig { provider, model, sampling, system, tool_policy, loop_config } = config;
    let LoopConfig {
        max_rounds,
        stop_condition,
        loop_detection,
        recovery,
        pre_round,
        stream_inactivity_timeout,
    } = loop_config;

    // Current system prompt; may be overridden per-round by `pre_round`. The
    // run-owned history (`messages`) never stores the system message — it is
    // prepended into each `CompletionRequest` by `build_request`. A caller may
    // still include their own `Message::System(_)` in history; those take
    // precedence and suppress auto-prepend.
    let mut current_system: String = system;

    let mut detector = LoopDetector::new(loop_detection);
    let tool_schemas = tools.definitions();

    let mut total_usage = Usage::default();
    let mut last_assistant: Option<Message> = None;
    let mut finish = FinishReason::Stopped;
    let mut rounds_done: u32 = 0;

    for round in 1..=max_rounds {
        if cancel.try_recv().is_ok() {
            fail(&events, &mut finish, AgentError::Cancelled, true);
            break;
        }

        if let Some(hook) = pre_round.as_ref() {
            // Guard against hook panics: `catch_unwind` surfaces them as a
            // terminal error on the event stream instead of letting the driver
            // task unwind silently.
            let hook_fut = std::panic::AssertUnwindSafe(hook(round)).catch_unwind();
            let hook_res = tokio::select! {
                biased;
                _ = &mut cancel => {
                    fail(&events, &mut finish, AgentError::Cancelled, true);
                    break;
                }
                v = hook_fut => v,
            };
            let out = match hook_res {
                Ok(v) => v,
                Err(panic) => {
                    let msg = panic_message(&panic);
                    fail(
                        &events,
                        &mut finish,
                        AgentError::Internal(format!("pre_round hook panicked: {msg}")),
                        false,
                    );
                    break;
                }
            };
            let PreRoundOutput { inject_messages, system: fresh_system } = out;
            messages.extend(inject_messages);
            if let Some(s) = fresh_system {
                current_system = s;
            }
        }

        let request =
            build_request(&model, &sampling, &current_system, &messages, &tool_schemas);

        let stream_result = open_stream(&*provider, &request, &recovery).await;
        let stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                fail(&events, &mut finish, AgentError::from(e), false);
                break;
            }
        };

        let out_res = tokio::select! {
            biased;
            _ = &mut cancel => {
                fail(&events, &mut finish, AgentError::Cancelled, true);
                break;
            }
            r = run_round(RoundInput {
                round,
                stream,
                tools: tools.as_ref(),
                approval: approval.as_ref(),
                tool_policy: &tool_policy,
                events: &events,
                stream_inactivity_timeout,
            }) => r,
        };

        let RoundOutput {
            assistant,
            tool_results,
            tool_calls,
            tool_call_records: records,
            usage,
            text_len,
            denied_count,
        } = match out_res {
            Ok(o) => o,
            Err(re) => {
                fail(&events, &mut finish, AgentError::from(re), false);
                break;
            }
        };

        total_usage.prompt_tokens += usage.prompt_tokens;
        total_usage.completion_tokens += usage.completion_tokens;
        if usage.total_tokens > 0 {
            total_usage.total_tokens += usage.total_tokens;
        } else {
            total_usage.total_tokens += usage.prompt_tokens + usage.completion_tokens;
        }

        last_assistant = Some(assistant.clone());
        messages.push(assistant);
        messages.extend(tool_results);

        rounds_done = round;

        let had_tool_calls = !tool_calls.is_empty();
        let tool_call_count = u32::try_from(tool_calls.len()).unwrap_or_else(|_| {
            tracing::warn!(n = tool_calls.len(), "tool_call_count exceeds u32::MAX; saturating");
            u32::MAX
        });
        let summary = RoundSummary {
            round,
            had_tool_calls,
            assistant_text_len: text_len,
            tool_call_count,
            denied_count,
        };

        let _ = events.send(AgentEvent::RoundEnded { round, had_tool_calls });

        if let Some(reason) = detector.check(&tool_calls) {
            // LoopDetected is a terminal failure: emit AgentEvent::Error so
            // observers see the underlying cause, but use the dedicated
            // FinishReason::LoopDetected variant rather than Error(_).
            let ae = Arc::new(AgentError::LoopDetected(reason));
            let _ = events.send(AgentEvent::Error(Arc::clone(&ae)));
            finish = FinishReason::LoopDetected;
            break;
        }

        if should_stop(&stop_condition, &summary, &records, round, max_rounds) {
            finish = FinishReason::Stopped;
            break;
        }

        if round == max_rounds {
            // Same shape as LoopDetected above: surface the underlying error
            // on the event stream but keep the dedicated terminal reason.
            let ae = Arc::new(AgentError::MaxRounds(max_rounds));
            let _ = events.send(AgentEvent::Error(Arc::clone(&ae)));
            finish = FinishReason::MaxRounds;
            break;
        }
    }

    if events.send(AgentEvent::Finished(finish.clone())).is_err() {
        tracing::debug!("event receiver dropped before Finished was observed");
    }

    DriveResult {
        outcome: RunOutcome {
            last_assistant,
            usage: total_usage,
            rounds: rounds_done,
            finish_reason: finish,
        },
        messages,
    }
}

fn build_request(
    model: &lutin_llm::ModelId,
    sampling: &SamplingParams,
    system: &str,
    messages: &[Message],
    tools: &[lutin_llm::ToolDefinition],
) -> CompletionRequest {
    // Prepend the current system prompt unless the caller embedded their own.
    // Why: a single clone of `messages` either way; pre-size the Vec so the
    // system-prepend path doesn't reallocate on extend.
    let has_system = messages.iter().any(|m| matches!(m, Message::System(_)));
    let prepend = !system.is_empty() && !has_system;
    let mut out_msgs: Vec<Message> = Vec::with_capacity(messages.len() + usize::from(prepend));
    if prepend {
        out_msgs.push(Message::System(system.to_string()));
    }
    out_msgs.extend(messages.iter().cloned());
    // Why: reasoning + response_format + ignore_providers moved into
    // `CompletionRequest::extensions` after the agent crate was written.
    let reasoning = sampling
        .reasoning
        .as_ref()
        .map(|r| lutin_llm::Reasoning {
            effort: r.effort,
            max_tokens: r.max_tokens,
        });
    CompletionRequest {
        model: model.clone(),
        messages: out_msgs,
        tools: tools.to_vec(),
        temperature: sampling.temperature,
        max_tokens: sampling.max_tokens,
        thinking_enabled: sampling.thinking_enabled,
        extensions: lutin_llm::Extensions {
            reasoning,
            response_format: None,
            ignore_providers: Vec::new(),
        },
    }
}

async fn open_stream(
    provider: &dyn lutin_llm::LlmProvider,
    request: &CompletionRequest,
    recovery: &crate::loop_control::RecoveryPolicy,
) -> Result<lutin_llm::EventStream, lutin_llm::LlmError> {
    let mut budget = RetryBudget::new(recovery.clone());
    let mut delay = INITIAL_RETRY_DELAY;
    loop {
        match provider.stream(request.clone()).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                if !budget.should_retry(&e) {
                    return Err(e);
                }
                tracing::warn!(error = %e, delay_ms = delay.as_millis() as u64, "retrying transient provider error");
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(MAX_RETRY_DELAY);
            }
        }
    }
}

fn should_stop(
    cond: &StopCondition,
    summary: &RoundSummary,
    records: &[ToolCallRecord],
    round: u32,
    max: u32,
) -> bool {
    match cond {
        StopCondition::NoToolCalls => !summary.had_tool_calls,
        StopCondition::MaxRounds => round >= max,
        StopCondition::ToolCalled(name) => {
            records.iter().any(|r| {
                &r.name == name
                    && r.outcome == crate::loop_control::ToolCallOutcome::Ok
            })
        }
        StopCondition::AnyCallDenied => summary.denied_count > 0,
        StopCondition::Custom(f) => f(summary, records),
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}
