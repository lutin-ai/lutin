use anyhow::{Result, anyhow};
use lutin_llm::{
    CallId, CompletionRequest, Message, ToolCall, ToolDefinition, ToolName, ToolParameter,
    ToolResultContent,
};

use tracing::info;

use crate::runtime::{build_plan_description, emit};
use crate::types::{Agent, AgentState, StepRecord};
use crate::wire::{ChatEvent, StepId};

pub const SUMMARY_TOOL: &str = "summary";
const SUMMARIZE_MAX_ATTEMPTS: usize = 10;

pub fn summary_tool_def() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new(SUMMARY_TOOL),
        description: "Submit a slightly detailed summary of the step that just ran. \
                      Cover what was attempted, what changed, and any facts the next step \
                      will need. Aim for a short paragraph (3-6 sentences) — concise, \
                      but more than a one-line digest."
            .into(),
        parameters: vec![ToolParameter {
            name: "summary".into(),
            r#type: "string".into(),
            description: "The summary text.".into(),
            required: true,
        }],
    }
}

pub(super) async fn run_summarize_stage(agent: &mut Agent, step_id: StepId) -> Result<()> {
    let (plan, output) = match &agent.state {
        AgentState::Summarize { plan, output } => (plan.clone(), output.clone()),
        _ => unreachable!(),
    };

    let step_call_id = CallId::new(format!("step-{}", agent.steps.len()));

    let plan_desc = match build_plan_description(&plan) {
        Message::User(s) => s,
        _ => String::new(),
    };
    let args_str = serde_json::to_string(&plan.args).unwrap_or_else(|_| "{}".into());
    let user_msg = format!(
        "A tool step just completed. Summarize it via the `summary` tool.\n\n\
         {plan_desc}\n\n\
         Tool invoked: `{}`\n\
         Args: {args_str}\n\n\
         Tool output:\n{output}",
        plan.tool,
    );

    let mut messages: Vec<Message> = Vec::with_capacity(3);
    if !agent.summarizer_system.is_empty() {
        messages.push(Message::System(agent.summarizer_system.clone()));
    }
    messages.push(Message::User(user_msg));

    let tools = vec![summary_tool_def()];
    let mut summary: Option<String> = None;

    info!(
        plan_tool = %plan.tool,
        msg_count = messages.len(),
        "summarize: starting stage"
    );

    for attempt in 0..SUMMARIZE_MAX_ATTEMPTS {
        let request = CompletionRequest {
            model: agent.summarizer_model.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            temperature: agent.summarizer_temperature,
            presence_penalty: agent.summarizer_presence_penalty,
            max_tokens: None,
            thinking_enabled: false,
            extensions: Default::default(),
        };
        let response = agent.summarizer_provider.complete(request).await?;

        info!(
            attempt,
            text = %response.text,
            tool_calls = ?response.tool_calls,
            thinking = ?response.thinking,
            "summarize: LLM response"
        );

        let tool_calls: Vec<_> = response.tool_calls.into_iter().take(1).collect();
        messages.push(Message::Assistant {
            text: response.text.clone(),
            tool_calls: tool_calls.clone(),
            thinking: response.thinking.clone(),
        });

        let Some(call) = tool_calls.first() else {
            messages.push(Message::User(
                "No tool call. You must call the `summary` tool.".into(),
            ));
            continue;
        };

        if call.name.as_str() != SUMMARY_TOOL {
            messages.push(Message::ToolResult(ToolResultContent {
                call_id: call.id.clone(),
                content: format!(
                    "`{}` is not available. Only `summary` can be called here.",
                    call.name.as_str()
                ),
                is_error: true,
            }));
            continue;
        }

        match call.arguments.get("summary").and_then(|v| v.as_str()) {
            Some(s) => {
                summary = Some(s.to_string());
                break;
            }
            None => {
                messages.push(Message::ToolResult(ToolResultContent {
                    call_id: call.id.clone(),
                    content: "`summary` arg missing or not a string. Call `summary` again with \
                              a string `summary` field."
                        .into(),
                    is_error: true,
                }));
                continue;
            }
        }
    }

    let summary = summary.ok_or_else(|| {
        anyhow!("summarize stage: model did not call `summary` after {SUMMARIZE_MAX_ATTEMPTS} attempts")
    })?;

    agent.messages.push(Message::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: step_call_id.clone(),
            name: ToolName::new(&plan.tool),
            arguments: plan.args.clone(),
        }],
        thinking: None,
    });
    agent.messages.push(Message::ToolResult(ToolResultContent {
        call_id: step_call_id.clone(),
        content: output,
        is_error: false,
    }));

    emit(
        agent,
        ChatEvent::SummarizeCompleted {
            step_id,
            summary: summary.clone(),
        },
    );

    agent.steps.push(StepRecord {
        call_id: step_call_id,
        plan,
        summary,
    });
    agent.state = AgentState::Done;
    Ok(())
}
