use anyhow::{Result, anyhow};
use lutin_llm::{
    CompletionRequest, Message, ToolDefinition, ToolName, ToolParameter, ToolResultContent,
};

use tracing::info;

use crate::runtime::emit;
use crate::runtime::trace::{log_messages, log_response};
use crate::types::{Agent, AgentState, Plan};
use crate::wire::{ChatEvent, StepId, WirePlan};

pub const PLAN_TOOL: &str = "plan";
pub const FINISH_TOOL: &str = "finish";
const PLAN_MAX_ATTEMPTS: usize = 10;

const PLAN_STAGE_PROMPT: &str = "Plan the next tool call using the `plan` tool, or call `finish` if the users request is already satisfied and no further tool calls are needed.";

pub fn finish_tool_def() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new(FINISH_TOOL),
        description: "End the current turn with a final reply to the user. Use only \
                      when no further tool calls are needed — e.g. the request is \
                      satisfied, or you're answering a question that needs no action."
            .into(),
        parameters: vec![ToolParameter {
            name: "reply".into(),
            r#type: "string".into(),
            description: "The message the user will see.".into(),
            required: true,
        }],
    }
}

pub fn plan_tool_def() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new(PLAN_TOOL),
        description: "Plan the very next step. Other tools are visible \
                      but disabled until you call this."
            .into(),
        parameters: vec![
            ToolParameter {
                name: "tool".into(),
                r#type: "string".into(),
                description: "Name of the single tool to run next step.".into(),
                required: true,
            },
            ToolParameter {
                name: "goal".into(),
                r#type: "string".into(),
                description: "The goal of this step. A concrete task for only this task, not the overall task."
                    .into(),
                required: true,
            },
            ToolParameter {
                name: "considerations".into(),
                r#type: "array".into(),
                description: "Constraints / facts the iteration agent should respect when \
                              drafting args (e.g. specific paths, formats, values that must \
                              appear)."
                    .into(),
                required: false,
            },
        ],
    }
}

pub(super) async fn run_plan_stage(agent: &mut Agent, step_id: StepId) -> Result<()> {
    let rethink_feedback = match &agent.state {
        AgentState::Plan { rethink_feedback } => rethink_feedback.clone(),
        _ => unreachable!(),
    };
    if let Some(feedback) = &rethink_feedback {
        emit(
            agent,
            ChatEvent::PlanRethink {
                step_id,
                feedback: feedback.clone(),
            },
        );
    }

    let baseline = agent.messages.len();
    let prompt = match rethink_feedback {
        Some(feedback) => format!(
            "{PLAN_STAGE_PROMPT}\n\nThe previous plan was rejected during iteration with this feedback — fold it into the new plan:\n  {feedback}"
        ),
        None => PLAN_STAGE_PROMPT.into(),
    };
    agent.messages.push(Message::User(prompt));

    let mut tools = agent.toolbox.definitions();
    tools.push(plan_tool_def());
    tools.push(finish_tool_def());

    for attempt in 0..PLAN_MAX_ATTEMPTS {
        info!(
            stage = "plan",
            attempt,
            step_id = step_id.0,
            "main agent: request"
        );
        log_messages("plan", &agent.messages);
        let request = CompletionRequest {
            model: agent.model.clone(),
            messages: agent.messages.clone(),
            tools: tools.clone(),
            temperature: agent.temperature,
            presence_penalty: agent.presence_penalty,
            max_tokens: None,
            thinking_enabled: false,
            extensions: Default::default(),
        };
        let response = agent.provider.complete(request).await?;
        log_response("plan", attempt, &response);

        let tool_calls: Vec<_> = response.tool_calls.into_iter().take(1).collect();
        agent.messages.push(Message::Assistant {
            text: response.text.clone(),
            tool_calls: tool_calls.clone(),
            thinking: response.thinking.clone(),
        });

        let Some(call) = tool_calls.first() else {
            agent.messages.push(Message::User(
                "No tool call detected. You must call the `plan` tool to choose which tool to run."
                    .into(),
            ));
            continue;
        };

        if call.name.as_str() == FINISH_TOOL {
            match call.arguments.get("reply").and_then(|v| v.as_str()) {
                Some(reply) => {
                    let reply = reply.to_string();
                    agent.messages.truncate(baseline);
                    agent.state = AgentState::AwaitInput { reply };
                    return Ok(());
                }
                None => {
                    agent.messages.push(Message::ToolResult(ToolResultContent {
                        call_id: call.id.clone(),
                        content: "`finish.reply` missing or not a string. Call `finish` again \
                                  with a string `reply`, or call `plan` to continue."
                            .into(),
                        is_error: true,
                    }));
                    continue;
                }
            }
        }

        if call.name.as_str() != PLAN_TOOL {
            agent.messages.push(Message::ToolResult(ToolResultContent {
                call_id: call.id.clone(),
                content: format!(
                    "`{}` is disabled at the planning stage. Call `plan` first with tool=\"{}\" \
                     to commit to it; the args will be drafted in the next stage.",
                    call.name.as_str(),
                    call.name.as_str(),
                ),
                is_error: true,
            }));
            continue;
        }

        let plan = match parse_plan_args(&call.arguments) {
            Ok(p) => p,
            Err(e) => {
                agent.messages.push(Message::ToolResult(ToolResultContent {
                    call_id: call.id.clone(),
                    content: format!("{e}. Call `plan` again with all required string fields."),
                    is_error: true,
                }));
                continue;
            }
        };
        if plan.tool == PLAN_TOOL || plan.tool == FINISH_TOOL {
            agent.messages.push(Message::ToolResult(ToolResultContent {
                call_id: call.id.clone(),
                content: format!(
                    "`{}` is a planning-stage control tool, not a step tool. Call `plan` again \
                     with `tool` set to one of the available step tools.",
                    plan.tool,
                ),
                is_error: true,
            }));
            continue;
        }
        let defs = agent.toolbox.definitions();
        if !defs.iter().any(|d| d.name.as_str() == plan.tool) {
            let available: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
            agent.messages.push(Message::ToolResult(ToolResultContent {
                call_id: call.id.clone(),
                content: format!(
                    "`{}` is not an available tool. Call `plan` again with `tool` set to one of: {}.",
                    plan.tool,
                    available.join(", "),
                ),
                is_error: true,
            }));
            continue;
        }
        emit(
            agent,
            ChatEvent::PlanProposed {
                step_id,
                plan: WirePlan::from_plan(&plan),
            },
        );
        agent.messages.truncate(baseline);
        agent.state = AgentState::Iterate {
            plan,
            fix_log: Vec::new(),
        };
        return Ok(());
    }

    Err(anyhow!(
        "plan stage: model did not call `plan` after {PLAN_MAX_ATTEMPTS} attempts"
    ))
}

fn parse_plan_args(args: &serde_json::Value) -> Result<Plan> {
    let obj = args
        .as_object()
        .ok_or_else(|| anyhow!("plan args must be a JSON object, got {args}"))?;
    let tool = obj
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("plan.tool missing or not a string"))?
        .to_string();
    let goal = obj
        .get("goal")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("plan.goal missing or not a string"))?
        .to_string();
    // let considerations = obj
    //     .get("considerations")
    //     .and_then(|v| v.as_array())
    //     .map(|arr| {
    //         arr.iter()
    //             .filter_map(|v| v.as_str().map(str::to_string))
    //             .collect()
    //     })
    //     .unwrap_or_default();
    Ok(Plan::new(tool, goal, vec![]))
}
