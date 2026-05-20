use anyhow::Result;
use lutin_llm::{CallId, ToolCall, ToolName};
use lutin_tools::{ToolCallContext, ToolResult};

use crate::runtime::emit;
use crate::types::{Agent, AgentState};
use crate::wire::{ChatEvent, StepId, WirePlan};

pub(super) async fn run_execute_stage(agent: &mut Agent, step_id: StepId) -> Result<()> {
    let plan = match &agent.state {
        AgentState::Execute { plan } => plan.clone(),
        _ => unreachable!(),
    };

    emit(
        agent,
        ChatEvent::ExecuteStarted {
            step_id,
            plan: WirePlan::from_plan(&plan),
        },
    );

    let step_call_id = CallId::new(format!("step-{}", agent.steps.len()));
    let call = ToolCall {
        id: step_call_id,
        name: ToolName::new(&plan.tool),
        arguments: plan.args.clone(),
    };
    let ctx = ToolCallContext::default();
    let result = agent.toolbox.call(&ctx, call).await;

    let output = match result {
        ToolResult::Ok(rc) => rc.content,
        ToolResult::Err(e) => format!("[tool error] {e}"),
        _ => "[tool error] unknown ToolResult variant".into(),
    };

    emit(
        agent,
        ChatEvent::ExecuteCompleted {
            step_id,
            output: output.clone(),
        },
    );

    agent.state = AgentState::Summarize { plan, output };
    Ok(())
}
