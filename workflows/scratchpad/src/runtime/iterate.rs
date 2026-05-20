use anyhow::{Result, anyhow};
use lutin_llm::{CallId, CompletionRequest, Message, ToolCall, ToolName, ToolResultContent};
use tracing::info;

use crate::runtime::trace::{log_messages, log_response};
use crate::runtime::{build_plan_description, emit};
use crate::types::{Agent, AgentState, FixEntry, Plan, Principle, Verdict};
use crate::wire::{ChatEvent, StepId, WireVerdict};

const ITERATE_HARD_CAP: usize = 50;
const DRAFT_MAX_ATTEMPTS: usize = 10;
const FIX_LOG_MAX: usize = 3;
const SYNTH_CALL_ID: &str = "iterate-call";

pub(super) async fn run_iterate_stage(
    agent: &mut Agent,
    principles: &[Principle],
    step_id: StepId,
) -> Result<()> {
    let baseline = agent.messages.len();
    let history: Vec<Message> = agent.messages[..baseline].to_vec();

    let plan_snapshot = match &agent.state {
        AgentState::Iterate { plan, .. } => plan.clone(),
        _ => unreachable!(),
    };
    agent.messages.push(build_plan_description(&plan_snapshot));
    let iter_baseline = agent.messages.len();

    let mut needs_redraft = true;
    for iter_idx in 0..ITERATE_HARD_CAP {
        let (plan, fix_log) = match &agent.state {
            AgentState::Iterate { plan, fix_log } => (plan.clone(), fix_log.clone()),
            _ => unreachable!(),
        };

        agent.messages.truncate(iter_baseline);

        let updated_plan = if needs_redraft {
            push_iterate_call_and_log(&mut agent.messages, &plan, &fix_log, principles);
            match draft_args(agent, &plan).await? {
                DraftOutcome::Drafted(new_args) => {
                    let mut p = plan.clone();
                    p.args = new_args;
                    p
                }
                DraftOutcome::SwitchTool { wanted } => {
                    let combined = format!(
                        "During iteration, you tried to call `{wanted}` instead of the \
                         planned `{}`. Re-plan: either pick `{wanted}` (or a closer fit) as the \
                         tool, or — if `{}` is still right — restate the goal so it's clear \
                         what {} should produce in one call.",
                        plan.tool, plan.tool, plan.tool
                    );
                    agent.messages.truncate(baseline);
                    agent.state = AgentState::Plan {
                        rethink_feedback: Some(combined),
                    };
                    return Ok(());
                }
            }
        } else {
            plan.clone()
        };
        needs_redraft = false;
        let args_str = serde_json::to_string(&updated_plan.args).unwrap_or_else(|_| "null".into());
        emit(
            agent,
            ChatEvent::IterationStarted {
                step_id,
                index: iter_idx as u32,
                args: args_str.clone(),
            },
        );
        emit(
            agent,
            ChatEvent::ScratchpadEdited {
                step_id,
                args: args_str,
            },
        );

        let mut verdict = Verdict::Pass;
        let mut offender: Option<String> = None;
        if iter_idx > 0 {
            for principle in principles {
                if !principle.applies_to.is_empty()
                    && !principle.applies_to.iter().any(|t| t == &updated_plan.tool)
                {
                    continue;
                }
                emit(
                    agent,
                    ChatEvent::CurrentPrincipleChanged {
                        step_id,
                        principle: Some(principle.name.clone()),
                    },
                );
                let persona = lutin_entities::Persona::load(&agent.resolver, &principle.persona)
                    .map_err(|e| {
                        anyhow!(
                            "load persona `{}` for principle `{}`: {e}",
                            principle.persona,
                            principle.name
                        )
                    })?;
                let v = crate::reviewer::review_principle(
                    agent.provider.as_ref(),
                    &agent.model,
                    &persona,
                    principle,
                    &updated_plan,
                    &history,
                )
                .await?;
                let wire_v = match &v {
                    Verdict::Pass => WireVerdict::Pass,
                    Verdict::Fix(f) => WireVerdict::Fix {
                        feedback: f.clone(),
                    },
                    Verdict::Rethink(f) => WireVerdict::Rethink {
                        feedback: f.clone(),
                    },
                };
                emit(
                    agent,
                    ChatEvent::PrincipleEvaluated {
                        step_id,
                        iteration: iter_idx as u32,
                        principle: principle.name.clone(),
                        verdict: wire_v,
                    },
                );
                if !matches!(v, Verdict::Pass) {
                    verdict = v;
                    offender = Some(principle.name.clone());
                    break;
                }
            }
        }
        emit(
            agent,
            ChatEvent::CurrentPrincipleChanged {
                step_id,
                principle: None,
            },
        );

        match verdict {
            Verdict::Pass if iter_idx == 0 => {
                agent.state = AgentState::Iterate {
                    plan: updated_plan,
                    fix_log,
                };
            }
            Verdict::Pass => {
                agent.messages.truncate(baseline);
                agent.state = AgentState::Execute { plan: updated_plan };
                return Ok(());
            }
            Verdict::Fix(feedback) => {
                let mut new_fix_log = fix_log;
                new_fix_log.push(FixEntry {
                    principle: offender.unwrap_or_default(),
                    feedback,
                });
                if new_fix_log.len() > FIX_LOG_MAX {
                    let overflow = new_fix_log.len() - FIX_LOG_MAX;
                    new_fix_log.drain(0..overflow);
                }
                emit(
                    agent,
                    ChatEvent::FixLogUpdated {
                        step_id,
                        fix_log: new_fix_log.clone(),
                    },
                );
                agent.state = AgentState::Iterate {
                    plan: updated_plan,
                    fix_log: new_fix_log,
                };
                needs_redraft = true;
            }
            Verdict::Rethink(feedback) => {
                let args_str =
                    serde_json::to_string(&updated_plan.args).unwrap_or_else(|_| "{}".into());
                let combined = format!(
                    "{feedback}\n\nThe args you had drafted before the rethink request:\n  {args_str}"
                );
                agent.messages.truncate(baseline);
                agent.state = AgentState::Plan {
                    rethink_feedback: Some(combined),
                };
                return Ok(());
            }
        }
    }

    Err(anyhow!(
        "iterate stage: hit hard cap of {ITERATE_HARD_CAP} iterations without converging"
    ))
}

fn push_iterate_call_and_log(
    messages: &mut Vec<Message>,
    plan: &Plan,
    fix_log: &[FixEntry],
    principles: &[Principle],
) {
    messages.push(Message::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: CallId::new(SYNTH_CALL_ID),
            name: ToolName::new(&plan.tool),
            arguments: plan.args.clone(),
        }],
        thinking: None,
    });

    let feedback_content = match fix_log.last() {
        Some(entry) => format!("[{}] {}", entry.principle, entry.feedback),
        None => "Tool call planned. Run again to update args.".into(),
    };
    messages.push(Message::ToolResult(ToolResultContent {
        call_id: CallId::new(SYNTH_CALL_ID),
        content: feedback_content,
        is_error: true,
    }));

    if !fix_log.is_empty() {
        let mut s = String::from(
            "Recently engaged principles — keep them satisfied while you address the current concern:\n",
        );
        for entry in fix_log {
            let p = principles.iter().find(|p| p.name == entry.principle);
            match p {
                Some(p) => s.push_str(&format!("  - {}: {}\n", p.title, p.description)),
                None => s.push_str(&format!("  - {}\n", entry.principle)),
            }
        }
        messages.push(Message::User(s));
    }
}

pub(super) enum DraftOutcome {
    Drafted(serde_json::Value),
    SwitchTool { wanted: String },
}

async fn draft_args(agent: &mut Agent, plan: &Plan) -> Result<DraftOutcome> {
    let tool_def = agent
        .toolbox
        .definitions()
        .into_iter()
        .find(|d| d.name.as_str() == plan.tool)
        .ok_or_else(|| anyhow!("planned tool `{}` not in toolbox", plan.tool))?;
    let tools = vec![tool_def];

    for attempt in 0..DRAFT_MAX_ATTEMPTS {
        info!(stage = "iterate", attempt, plan_tool = %plan.tool, "main agent: request");
        log_messages("iterate", &agent.messages);
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
        log_response("iterate", attempt, &response);

        let tool_calls: Vec<_> = response.tool_calls.into_iter().take(1).collect();
        agent.messages.push(Message::Assistant {
            text: response.text.clone(),
            tool_calls: tool_calls.clone(),
            thinking: response.thinking.clone(),
        });

        let Some(call) = tool_calls.first() else {
            agent.messages.push(Message::User(format!(
                "No tool call detected. Call `{}` with updated args.",
                plan.tool
            )));
            continue;
        };

        if call.name.as_str() != plan.tool {
            // Model wants a different tool. Don't fight it — bubble up
            // so iterate can re-plan with the wanted tool as feedback.
            return Ok(DraftOutcome::SwitchTool {
                wanted: call.name.as_str().to_string(),
            });
        }

        return Ok(DraftOutcome::Drafted(call.arguments.clone()));
    }

    Err(anyhow!(
        "iterate draft: model did not call `{}` after {DRAFT_MAX_ATTEMPTS} attempts",
        plan.tool
    ))
}
