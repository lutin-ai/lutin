mod execute;
mod iterate;
mod plan;
mod summarize;
pub(crate) mod trace;

use anyhow::Result;
use lutin_llm::Message;

use crate::types::{Agent, AgentState, Plan, Principle, StepOutcome};
use crate::wire::{ChatEvent, StepId};

pub use plan::{FINISH_TOOL, PLAN_TOOL, finish_tool_def, plan_tool_def};
pub use summarize::{SUMMARY_TOOL, summary_tool_def};

pub async fn run_step(agent: &mut Agent, principles: &[Principle]) -> Result<StepOutcome> {
    let step_id = StepId(agent.steps.len() as u64);
    emit(agent, ChatEvent::StepStarted { step_id });
    loop {
        match &agent.state {
            AgentState::Plan { .. } => plan::run_plan_stage(agent, step_id).await?,
            AgentState::Iterate { .. } => {
                iterate::run_iterate_stage(agent, principles, step_id).await?
            }
            AgentState::Execute { .. } => execute::run_execute_stage(agent, step_id).await?,
            AgentState::Summarize { .. } => summarize::run_summarize_stage(agent, step_id).await?,
            AgentState::Done => {
                emit(agent, ChatEvent::StepCompleted { step_id });
                agent.state = AgentState::Plan {
                    rethink_feedback: None,
                };
                return Ok(StepOutcome::Continue);
            }
            AgentState::AwaitInput { reply } => {
                let reply = reply.clone();
                emit(agent, ChatEvent::StepCompleted { step_id });
                agent.state = AgentState::Plan {
                    rethink_feedback: None,
                };
                return Ok(StepOutcome::Yield { reply });
            }
        }
    }
}

pub(crate) fn emit(agent: &Agent, event: ChatEvent) {
    let _ = agent.events.send(event);
}

// pub(crate) fn build_plan_description(plan: &Plan) -> Message {
//     let mut s = format!(
//         "[iteration stage]\nI'm going to call `{}`.\nGoal: {}\nWhy this tool: {}",
//         plan.tool, plan.goal, plan.why_this_tool
//     );
//     if !plan.considerations.is_empty() {
//         s.push_str("\nConsiderations:");
//         for c in &plan.considerations {
//             s.push_str(&format!("\n  - {c}"));
//         }
//     }
//     Message::Assistant {
//         text: s,
//         tool_calls: vec![],
//         thinking: None,
//     }
// }
pub(crate) fn build_plan_description(plan: &Plan) -> Message {
    Message::Assistant {
        text: format!(
            "I will now run the next step. I'm going to call `{}`.\nGoal: {}",
            plan.tool, plan.goal
        ),
        tool_calls: vec![],
        thinking: None,
    }
}
