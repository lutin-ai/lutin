use anyhow::{Result, anyhow};
use lutin_entities::Persona;
use lutin_llm::{
    CompletionRequest, LlmProvider, Message, ModelId, ToolDefinition, ToolName, ToolParameter,
};

use crate::types::{ContextItem, Principle, ReviewedCall, Verdict};

const VERDICT_TOOL: &str = "submit_verdict";
const MAX_ATTEMPTS: u32 = 3;
const REVIEWER_MAX_TOKENS: u32 = 8192;

pub async fn review_principle(
    provider: &dyn LlmProvider,
    fallback_model: &ModelId,
    persona: &Persona,
    principle: &Principle,
    call: &ReviewedCall,
    history: &[Message],
) -> Result<Verdict> {
    let system = build_system_prompt(persona, principle);
    let user = build_user_prompt(principle, call, history);
    let mut messages = vec![Message::System(system), Message::User(user)];

    let model = persona
        .model
        .as_deref()
        .map(ModelId::new)
        .unwrap_or_else(|| fallback_model.clone());

    let mut attempt: u32 = 0;
    loop {
        let request = CompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            tools: vec![submit_verdict_tool()],
            temperature: persona.temperature.or(Some(0.0)),
            presence_penalty: persona.presence_penalty,
            max_tokens: Some(REVIEWER_MAX_TOKENS),
            thinking_enabled: persona.thinking_enabled,
            extensions: Default::default(),
        };
        let response = provider.complete(request).await?;

        if let Some(call) = response.tool_calls.first()
            && call.name.as_str() == VERDICT_TOOL
        {
            return parse_verdict(&call.arguments);
        }

        attempt += 1;
        if attempt >= MAX_ATTEMPTS {
            return Err(anyhow!(
                "reviewer for `{}`: no `submit_verdict` after {MAX_ATTEMPTS} attempts",
                principle.name
            ));
        }
        messages.push(Message::Assistant {
            text: response.text.clone(),
            tool_calls: response.tool_calls.clone(),
            thinking: response.thinking.clone(),
        });
        messages.push(Message::User(
            "You did not call `submit_verdict`. Call it now with your verdict.".into(),
        ));
    }
}

fn build_system_prompt(persona: &Persona, principle: &Principle) -> String {
    let mut s = String::new();
    // if !persona.system_prompt.is_empty() {
    //     s.push_str(&persona.system_prompt);
    //     s.push_str("\n\n");
    // }
    s.push_str(
        "You are a single-principle reviewer. Judge ONLY the principle below; ignore every \
         other concern. Submit your verdict by calling the `submit_verdict` tool exactly once.\n\n",
    );
    s.push_str("PRINCIPLE: ");
    s.push_str(&principle.title);
    s.push_str("\n\n");
    s.push_str(&principle.description);
    s.push_str("\n\n");
    s.push_str("If the principle isn't applicable to the situation, set verdict to 'pass'.");
    s
}

fn build_user_prompt(principle: &Principle, call: &ReviewedCall, history: &[Message]) -> String {
    let mut s = String::new();

    if !call.goal.is_empty() {
        s.push_str("Agent's stated intent (from its assistant text):\n");
        s.push_str(&format!("  {}\n\n", call.goal));
    }

    s.push_str("Proposed tool call:\n");
    s.push_str(&format!("  name: {}\n", call.tool));
    let args = serde_json::to_string(&call.args).unwrap_or_else(|_| "<unserializable>".into());
    s.push_str(&format!("  arguments: {args}\n"));

    if principle.context.contains(&ContextItem::Chat) && !history.is_empty() {
        s.push_str("\nConversation history before this tool call:\n");
        for m in history {
            s.push_str(&render_message_for_reviewer(m));
        }
    }

    s.push_str("\nCall `submit_verdict` now.");
    s
}

fn render_message_for_reviewer(m: &Message) -> String {
    match m {
        Message::System(t) => format!("[system] {t}\n"),
        Message::User(t) => format!("[user] {t}\n"),
        Message::Assistant {
            text, tool_calls, ..
        } => {
            let mut s = format!("[assistant] {text}\n");
            for tc in tool_calls {
                let args = serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into());
                s.push_str(&format!("  -> tool_call {}({args})\n", tc.name.as_str()));
            }
            s
        }
        Message::ToolResult(rc) => {
            format!("[tool_result for {}] {}\n", rc.call_id.as_str(), rc.content)
        }
        Message::Image { .. } => "[image]\n".into(),
        Message::SubAgentReply { agent_id, text } => format!("[subagent#{agent_id}] {text}\n"),
        Message::SubAgentFailure { agent_id, reason } => {
            format!("[subagent#{agent_id} failed: {reason}]\n")
        }
        Message::Summary { text } => format!("[summary] {text}\n"),
    }
}

fn submit_verdict_tool() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new(VERDICT_TOOL),
        description: "Submit your verdict on the proposed tool call. Call exactly once.".into(),
        parameters: vec![
            ToolParameter {
                name: "verdict".into(),
                r#type: "string".into(),
                description:
                    "One of: \"pass\", \"fix\", \"rethink\". \"pass\" = principle satisfied. \
                     \"fix\" = retry with adjusted args. \"rethink\" = the planned tool is the \
                     wrong choice and the agent should re-plan."
                        .into(),
                required: true,
            },
            ToolParameter {
                name: "feedback".into(),
                r#type: "string".into(),
                description:
                    "Required for fix and rethink. Concrete guidance the agent should act on. \
                     Leave empty for pass."
                        .into(),
                required: false,
            },
        ],
    }
}

fn parse_verdict(args: &serde_json::Value) -> Result<Verdict> {
    let verdict = args
        .get("verdict")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("verdict missing or not a string"))?;
    let feedback = args
        .get("feedback")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match verdict {
        "pass" => Ok(Verdict::Pass),
        "fix" => Ok(Verdict::Fix(feedback)),
        "rethink" => Ok(Verdict::Rethink(feedback)),
        other => Err(anyhow!("unknown verdict: {other}")),
    }
}
