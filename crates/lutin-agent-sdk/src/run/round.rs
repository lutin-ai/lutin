use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;
use lutin_llm::{CallId, LlmError, Message, StreamEvent, ToolCall, ToolName, Usage};
use tokio::sync::mpsc;

use crate::{
    approval::{Approval, ApprovalPolicy},
    error::AgentError,
    event::AgentEvent,
    tools::{ToolCallContext, ToolError, ToolResult, ToolPolicy, Toolbox},
};

pub struct RoundInput<'a> {
    pub(crate) round: u32,
    pub(crate) stream: lutin_llm::EventStream,
    pub(crate) tools: &'a Toolbox,
    pub(crate) approval: &'a dyn ApprovalPolicy,
    pub(crate) tool_policy: &'a ToolPolicy,
    pub(crate) events: &'a mpsc::UnboundedSender<AgentEvent>,
    pub(crate) stream_inactivity_timeout: Option<Duration>,
}

pub struct RoundOutput {
    pub(crate) assistant: Message,
    pub(crate) tool_results: Vec<Message>,
    pub(crate) tool_calls: Vec<ToolCall>,
    /// Built inline during dispatch so callers don't have to zip parallel vecs.
    pub(crate) tool_call_records: Vec<crate::loop_control::ToolCallRecord>,
    pub(crate) usage: Usage,
    pub(crate) text_len: usize,
    pub(crate) denied_count: u32,
}

struct ToolFragment {
    name: ToolName,
    args_raw: String,
}

pub(crate) struct RoundAccumulator {
    text_buf: String,
    thinking_buf: Option<String>,
    tool_fragments: HashMap<CallId, ToolFragment>,
    order: Vec<CallId>,
    parse_errors: HashMap<CallId, String>,
}

pub(crate) struct FinalizedRound {
    pub text: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub parse_errors: HashMap<CallId, String>,
}

impl RoundAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            text_buf: String::new(),
            thinking_buf: None,
            tool_fragments: HashMap::new(),
            order: Vec::new(),
            parse_errors: HashMap::new(),
        }
    }

    pub(crate) fn push_text(&mut self, t: &str) {
        self.text_buf.push_str(t);
    }

    pub(crate) fn push_thinking(&mut self, t: &str) {
        self.thinking_buf.get_or_insert_with(String::new).push_str(t);
    }

    pub(crate) fn start_tool_call(&mut self, id: CallId, name: ToolName) {
        if !self.tool_fragments.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.tool_fragments
            .entry(id)
            .or_insert_with(|| ToolFragment { name, args_raw: String::new() });
    }

    pub(crate) fn push_delta(&mut self, id: CallId, arguments: &str) -> Result<(), CallId> {
        // Why: a delta without a prior ToolCallStart is a provider protocol violation; surface
        // it so recovery policy decides, rather than fabricating a synthetic empty-name call.
        let Some(entry) = self.tool_fragments.get_mut(&id) else {
            return Err(id);
        };
        entry.args_raw.push_str(arguments);
        Ok(())
    }

    pub(crate) fn finalize(mut self, max_calls: usize) -> FinalizedRound {
        let mut tool_calls: Vec<ToolCall> = Vec::with_capacity(self.order.len().min(max_calls));
        for id in self.order.into_iter().take(max_calls) {
            let Some(frag) = self.tool_fragments.remove(&id) else { continue };
            let arguments = if self.parse_errors.contains_key(&id) || frag.args_raw.is_empty() {
                serde_json::Value::Null
            } else {
                match serde_json::from_str(&frag.args_raw) {
                    Ok(v) => v,
                    Err(e) => {
                        self.parse_errors.insert(id.clone(), e.to_string());
                        serde_json::Value::Null
                    }
                }
            };
            tool_calls.push(ToolCall { id, name: frag.name, arguments });
        }
        FinalizedRound {
            text: self.text_buf,
            thinking: self.thinking_buf,
            tool_calls,
            parse_errors: self.parse_errors,
        }
    }
}

#[tracing::instrument(skip_all, fields(round = input.round))]
pub async fn run_round(input: RoundInput<'_>) -> Result<RoundOutput, RoundError> {
    let RoundInput {
        round,
        mut stream,
        tools,
        approval,
        tool_policy,
        events,
        stream_inactivity_timeout,
    } = input;

    let _ = events.send(AgentEvent::RoundStarted { round });

    let mut acc = RoundAccumulator::new();
    let mut usage = Usage::default();

    loop {
        let next = match stream_inactivity_timeout {
            Some(d) => match tokio::time::timeout(d, stream.next()).await {
                Ok(n) => n,
                Err(_) => return Err(RoundError::StreamStalled(d)),
            },
            None => stream.next().await,
        };
        let Some(evt) = next else { break };
        match evt {
            Ok(StreamEvent::Delta(t)) => {
                let _ = events.send(AgentEvent::AssistantText(t.clone()));
                acc.push_text(&t);
            }
            Ok(StreamEvent::Reasoning(t)) => {
                let _ = events.send(AgentEvent::AssistantReasoning(t.clone()));
                acc.push_thinking(&t);
            }
            Ok(StreamEvent::ToolCallStart { id, name }) => {
                acc.start_tool_call(id, name);
            }
            Ok(StreamEvent::ToolCallDelta { id, arguments }) => {
                if let Err(orphan_id) = acc.push_delta(id, &arguments) {
                    return Err(RoundError::Provider(LlmError::Stream(format!(
                        "orphan tool-call delta: id={} received without ToolCallStart",
                        orphan_id.as_str()
                    ))));
                }
            }
            Ok(StreamEvent::Done { usage: u }) => {
                if let Some(u) = u {
                    let _ = events.send(AgentEvent::Usage(u.clone()));
                    usage = u;
                }
            }
            Ok(StreamEvent::Provider(_)) => {}
            Err(e) => return Err(RoundError::Provider(e)),
        }
    }

    let max_calls = usize::try_from(tool_policy.max_calls_per_round).unwrap_or(usize::MAX);
    let FinalizedRound { text, thinking, tool_calls, mut parse_errors } = acc.finalize(max_calls);

    let text_len = text.len();
    let assistant = Message::Assistant {
        text,
        tool_calls: tool_calls.clone(),
        thinking,
    };
    let _ = events.send(AgentEvent::AssistantMessage(assistant.clone()));

    let mut tool_results: Vec<Message> = Vec::with_capacity(tool_calls.len());
    let mut tool_call_records: Vec<crate::loop_control::ToolCallRecord> =
        Vec::with_capacity(tool_calls.len());
    let mut denied_count: u32 = 0;
    for (idx, call) in tool_calls.iter().enumerate() {
        // Why: build Arc once per call; both events share it via cheap refcount bumps.
        let call_shared = std::sync::Arc::new(call.clone());
        let _ = events.send(AgentEvent::ToolCallStarted(std::sync::Arc::clone(&call_shared)));
        let outcome = if let Some(msg) = parse_errors.remove(&call.id) {
            ToolResult::Err(ToolError::InvalidArgs(msg))
        } else {
            match approval.decide(call).await {
                Approval::Deny(reason) => {
                    denied_count = denied_count.saturating_add(1);
                    ToolResult::Err(ToolError::Denied(reason.into_owned()))
                }
                Approval::Allow => {
                    let call_index = u32::try_from(idx).unwrap_or_else(|_| {
                        tracing::warn!(idx, "call_index exceeds u32::MAX; saturating");
                        u32::MAX
                    });
                    let ctx = ToolCallContext { round, call_index };
                    dispatch(tools, &ctx, call.clone(), tool_policy).await
                }
            }
        };
        let result_msg = outcome_to_message(&call.id, &outcome);
        let outcome_class = if matches!(outcome, ToolResult::Ok(_)) {
            crate::loop_control::ToolCallOutcome::Ok
        } else {
            crate::loop_control::ToolCallOutcome::Failed
        };
        tool_call_records.push(crate::loop_control::ToolCallRecord::new(
            call.name.clone(),
            call.id.clone(),
            outcome_class,
        ));
        let _ = events.send(AgentEvent::ToolCallCompleted {
            call: call_shared,
            outcome: outcome.clone(),
        });
        tool_results.push(result_msg);
    }

    Ok(RoundOutput {
        assistant,
        tool_results,
        tool_calls,
        tool_call_records,
        usage,
        text_len,
        denied_count,
    })
}

async fn dispatch(
    host: &Toolbox,
    ctx: &ToolCallContext,
    call: ToolCall,
    policy: &ToolPolicy,
) -> ToolResult {
    match policy.per_call_timeout {
        Some(d) => match tokio::time::timeout(d, host.call(ctx, call)).await {
            Ok(o) => o,
            Err(_) => ToolResult::Err(ToolError::Timeout),
        },
        None => host.call(ctx, call).await,
    }
}

fn outcome_to_message(call_id: &CallId, outcome: &ToolResult) -> Message {
    // Why: ToolResult is `#[non_exhaustive]`; cover unknown variants conservatively.
    match outcome {
        ToolResult::Ok(r) => Message::ToolResult(lutin_llm::ToolResultContent {
            call_id: call_id.clone(),
            content: r.content.clone(),
            is_error: r.is_error,
        }),
        ToolResult::Err(e) => Message::ToolResult(lutin_llm::ToolResultContent {
            call_id: call_id.clone(),
            content: e.to_string(),
            is_error: true,
        }),
        _ => Message::ToolResult(lutin_llm::ToolResultContent {
            call_id: call_id.clone(),
            content: "<unknown tool outcome>".into(),
            is_error: true,
        }),
    }
}

#[derive(Debug)]
pub enum RoundError {
    Provider(LlmError),
    StreamStalled(Duration),
}

impl From<RoundError> for AgentError {
    fn from(e: RoundError) -> Self {
        match e {
            RoundError::Provider(p) => AgentError::Provider(p),
            RoundError::StreamStalled(d) => AgentError::StreamStalled(d),
        }
    }
}
