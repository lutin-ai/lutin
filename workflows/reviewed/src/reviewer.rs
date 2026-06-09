use anyhow::{Result, anyhow};
use lutin_entities::Persona;
use lutin_llm::{
    CompletionRequest, LlmProvider, Message, ModelId, ToolCall, ToolDefinition, ToolName,
    ToolParameter, ToolResultContent,
};
use lutin_tools::{ToolCallContext, ToolResult, Toolbox};

use crate::types::{ContextItem, Principle, ReviewSubject, Verdict};

const PASS_TOOL: &str = "pass";
const FAIL_TOOL: &str = "fail";
const GATE_APPLIES: &str = "condition_met";
const GATE_SKIP: &str = "condition_not_met";
const MAX_ATTEMPTS: u32 = 3;
const REVIEWER_MAX_TOKENS: u32 = 8192;
/// History budget for a reviewer prompt. Unlike the agent loop, the reviewer
/// renders the whole conversation into a single user message and asks for an
/// 8192-token verdict on top, so it gets less than the agent's full input
/// budget: the shared limit minus room for the response and the reviewer's
/// own framing (action under review, principle, system prompt).
const REVIEWER_HISTORY_LIMIT: usize =
    crate::runtime::CONTEXT_TOKEN_LIMIT - REVIEWER_MAX_TOKENS as usize - 8_000;
/// Investigation budget per review: tool calls a reviewer may make before
/// it must issue a verdict.
const INVESTIGATE_MAX_STEPS: usize = 8;
/// Cap on a single investigation result inside a reviewer's context.
const INVESTIGATE_MAX_OUTPUT: usize = 12_000;

/// Judge a leaf principle: it calls either `pass` or `fail`.
pub async fn review_principle(
    provider: &dyn LlmProvider,
    fallback_model: &ModelId,
    persona: &Persona,
    principle: &Principle,
    subject: ReviewSubject<'_>,
    history: &[Message],
    tasks: Option<&str>,
    toolbox: &Toolbox,
) -> Result<Verdict> {
    let include_history = principle.context.contains(&ContextItem::Chat);
    let user = format!(
        "{}\n\n{}",
        situation_context(subject, history, include_history, tasks),
        leaf_principle_block(principle),
    );
    let tool_call = elicit_call(
        provider,
        fallback_model,
        persona,
        REVIEWER_SYSTEM.to_string(),
        user,
        verdict_tools(),
        &[PASS_TOOL, FAIL_TOOL],
        principle,
        investigation_tools(principle, toolbox),
        toolbox,
    )
    .await?;
    parse_verdict(&tool_call)
}

/// The relevance gate for a grouping principle: does its condition apply
/// to the drafted call? Only on `true` are the sub-principles checked.
pub async fn review_relevance(
    provider: &dyn LlmProvider,
    fallback_model: &ModelId,
    persona: &Persona,
    principle: &Principle,
    subject: ReviewSubject<'_>,
    history: &[Message],
    tasks: Option<&str>,
    toolbox: &Toolbox,
) -> Result<bool> {
    let include_history = principle.context.contains(&ContextItem::Chat);
    let user = format!(
        "{}\n\n{}",
        situation_context(subject, history, include_history, tasks),
        gate_principle_block(principle),
    );
    let tool_call = elicit_call(
        provider,
        fallback_model,
        persona,
        GATE_SYSTEM.to_string(),
        user,
        gate_tools(),
        &[GATE_APPLIES, GATE_SKIP],
        principle,
        Vec::new(),
        toolbox,
    )
    .await?;
    Ok(tool_call.name.as_str() == GATE_APPLIES)
}

/// The investigation tool definitions a principle is allowed: its `tools`
/// allowlist resolved against the main toolbox. Names the toolbox doesn't
/// have (e.g. memory tools when memory is disabled) are silently absent.
fn investigation_tools(principle: &Principle, toolbox: &Toolbox) -> Vec<ToolDefinition> {
    if principle.tools.is_empty() {
        return Vec::new();
    }
    toolbox
        .definitions()
        .into_iter()
        .filter(|d| principle.tools.iter().any(|t| t == d.name.as_str()))
        .collect()
}

/// Drive a reviewer until it calls one of `accepted` tools, nudging it
/// up to `MAX_ATTEMPTS` times if it answers in prose instead. When the
/// principle grants `investigation` tools, calls to them are executed
/// against the main toolbox (up to `INVESTIGATE_MAX_STEPS`) so the
/// reviewer can check the actual state of the project before judging.
#[allow(clippy::too_many_arguments)]
async fn elicit_call(
    provider: &dyn LlmProvider,
    fallback_model: &ModelId,
    persona: &Persona,
    system: String,
    user: String,
    verdicts: Vec<ToolDefinition>,
    accepted: &[&str],
    principle: &Principle,
    investigation: Vec<ToolDefinition>,
    toolbox: &Toolbox,
) -> Result<ToolCall> {
    let model = persona
        .model
        .as_deref()
        .map(ModelId::new)
        .unwrap_or_else(|| fallback_model.clone());
    let mut messages = vec![Message::System(system), Message::User(user)];

    let investigate_names: Vec<String> = investigation
        .iter()
        .map(|d| d.name.as_str().to_string())
        .collect();
    let mut tools = verdicts;
    tools.extend(investigation);

    let mut nudges: u32 = 0;
    let mut steps: usize = 0;
    loop {
        let request = CompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            temperature: persona.temperature.or(Some(0.0)),
            presence_penalty: persona.presence_penalty,
            max_tokens: Some(REVIEWER_MAX_TOKENS),
            thinking_enabled: persona.thinking_enabled,
            extensions: Default::default(),
        };
        let response = provider.complete(request).await?;

        if let Some(call) = response
            .tool_calls
            .iter()
            .find(|c| accepted.contains(&c.name.as_str()))
        {
            return Ok(call.clone());
        }

        if let Some(call) = response
            .tool_calls
            .iter()
            .find(|c| investigate_names.iter().any(|n| n == c.name.as_str()))
            .cloned()
        {
            messages.push(Message::Assistant {
                text: response.text.clone(),
                tool_calls: vec![call.clone()],
                thinking: response.thinking.clone(),
                thinking_signature: response.thinking_signature.clone(),
            });
            let (content, is_error) = if steps >= INVESTIGATE_MAX_STEPS {
                (
                    format!(
                        "[investigation budget exhausted — call one of: {} now]",
                        accepted.join(", ")
                    ),
                    true,
                )
            } else {
                steps += 1;
                match toolbox.call(&ToolCallContext::default(), call.clone()).await {
                    ToolResult::Ok(rc) => (clip(rc.content), rc.is_error),
                    ToolResult::Err(e) => (format!("[tool error] {e}"), true),
                    _ => ("[tool error] unknown ToolResult variant".into(), true),
                }
            };
            messages.push(Message::ToolResult(ToolResultContent {
                call_id: call.id.clone(),
                content,
                is_error,
            }));
            continue;
        }

        nudges += 1;
        if nudges >= MAX_ATTEMPTS {
            return Err(anyhow!(
                "reviewer for `{}`: no verdict tool after {MAX_ATTEMPTS} attempts",
                principle.name
            ));
        }
        messages.push(Message::Assistant {
            text: response.text.clone(),
            tool_calls: response.tool_calls.clone(),
            thinking: response.thinking.clone(),
            thinking_signature: response.thinking_signature.clone(),
        });
        messages.push(Message::User(format!(
            "You did not call one of: {}. Call it now.",
            accepted.join(", ")
        )));
    }
}

fn clip(mut s: String) -> String {
    if s.len() > INVESTIGATE_MAX_OUTPUT {
        s.truncate(INVESTIGATE_MAX_OUTPUT);
        s.push_str("\n…[truncated]");
    }
    s
}

// Generic reviewer system prompts — IDENTICAL for every leaf (resp. gate)
// on a trigger. The principle being judged is appended to the END of the
// user message (see `leaf_principle_block` / `gate_principle_block`), never
// baked in here. Combined with the uniform per-class tool set, this makes
// the whole leading prefix — system block + tool defs + the conversation
// transcript + the pending action — byte-identical across every principle
// agent on the same trigger, so vLLM's automatic prefix cache prefills that
// transcript ONCE per trigger and reuses it for every principle instead of
// re-prefilling it N times.
//
// HARD RULE: anything that varies per principle MUST live in the principle
// block (a pure suffix). Moving per-principle text ahead of the transcript
// would poison the shared prefix and silently kill the cache reuse.
//
// Exception, accepted knowingly: a principle with a `tools` allowlist gets
// extra tool definitions in its request, which forks its prefix off the
// shared one. Tag sparingly — every tagged principle pays its own prefill.
const REVIEWER_SYSTEM: &str =
    "You are a single-principle reviewer in a hierarchical principle system. You are shown the \
     current situation — the conversation so far and the action about to be taken — and then ONE \
     principle to judge at the very end. Judge ONLY that principle; ignore every other concern. \
     Some principles grant investigation tools (e.g. read, shell, recall); if any are available, \
     you may use them sparingly to check the actual state before judging. Then call `pass` if \
     the principle is satisfied or doesn't apply, or `fail` with a description of what's wrong \
     if it is violated. Call exactly one of them, exactly once.";

const GATE_SYSTEM: &str =
    "You are a relevance gate in a hierarchical principle system. You are shown the current \
     situation — the conversation so far and the action about to be taken — and then ONE \
     condition at the very end. Decide ONLY whether that condition applies to the proposed tool \
     call; do not judge the action's quality. Call `condition_met` if it applies (its \
     sub-principles will then be checked) or `condition_not_met` if it does not.";

/// The SHARED, principle-independent part of the user turn: the transcript
/// (for full-context checks), the agent's current task list, then the
/// pending action. Built identically for every principle on the same
/// trigger so it forms a cacheable prefix. The transcript comes FIRST — it
/// is the largest block and the most stable across turns — and the small
/// pending action comes last.
/// Keep the newest tail of `history` that fits `budget` estimated tokens,
/// dropping older messages. The newest message is always kept even if it
/// alone exceeds the budget. Rendered as prose for the reviewer, so a
/// dropped predecessor only loses context — it can't malform the request.
fn trim_history(history: &[Message], budget: usize) -> &[Message] {
    let mut used = 0usize;
    let mut start = history.len();
    for i in (0..history.len()).rev() {
        let t = crate::runtime::estimate_tokens(&history[i]);
        if start != history.len() && used + t > budget {
            break;
        }
        used += t;
        start = i;
    }
    &history[start..]
}

fn situation_context(
    subject: ReviewSubject<'_>,
    history: &[Message],
    include_history: bool,
    tasks: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if include_history && !history.is_empty() {
        let kept = trim_history(history, REVIEWER_HISTORY_LIMIT);
        let mut t = String::from("# Conversation so far\n");
        if kept.len() < history.len() {
            t.push_str("(earlier messages omitted to fit the context budget)\n");
        }
        for m in kept {
            t.push_str(&render_message_for_reviewer(m));
        }
        parts.push(t);
    }

    if let Some(tasks) = tasks {
        parts.push(format!("# Agent task list (current)\n{tasks}"));
    }

    let action = match subject {
        ReviewSubject::Tool(call) => {
            let mut action = String::from(
                "# Action under review\nThe agent is about to run a tool. Review the pending call \
                 BEFORE it executes:\n\n",
            );
            if !call.goal.is_empty() {
                action.push_str(&format!("intent: {}\n", call.goal));
            }
            action.push_str(&format!("tool: {}\n", call.tool));
            let args =
                serde_json::to_string_pretty(&call.args).unwrap_or_else(|_| "<unserializable>".into());
            action.push_str(&format!("args:\n{args}\n"));
            action
        }
        ReviewSubject::UserMessage(text) => format!(
            "# Action under review\nThe user just sent this message; review it BEFORE the agent \
             acts:\n\n{text}\n"
        ),
        ReviewSubject::AgentReply(text) => format!(
            "# Action under review\nThe agent is about to reply with this message; review it BEFORE \
             it is sent:\n\n{text}\n"
        ),
    };
    parts.push(action);

    parts.join("\n\n")
}

/// The VARYING tail for a leaf check: which principle to judge and how.
/// Appended AFTER `situation_context` so it is a pure suffix.
fn leaf_principle_block(principle: &Principle) -> String {
    let mut s = format!("# Principle to judge: {}\n", principle.title);
    if !principle.condition.is_empty() {
        s.push_str(&format!(
            "\n## Condition (when this principle applies)\n{}\n",
            principle.condition
        ));
    }
    if !principle.description.is_empty() {
        s.push_str(&format!("\n## Guidance\n{}\n", principle.description));
    }
    s.push_str(
        "\nCall `pass` if the principle is satisfied or doesn't apply, or `fail` with a concrete \
         description of what's wrong. Call one now.",
    );
    s
}

/// The VARYING tail for a gate: the condition to evaluate. Pure suffix.
fn gate_principle_block(principle: &Principle) -> String {
    let cond = if principle.condition.is_empty() {
        principle.description.as_str()
    } else {
        principle.condition.as_str()
    };
    format!(
        "# Condition to evaluate: {}\n\n{cond}\n\nDecide now: call `condition_met` or \
         `condition_not_met`.",
        principle.title
    )
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

fn verdict_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: ToolName::new(PASS_TOOL),
            description: "The principle is satisfied (or doesn't apply to this call). Takes no \
                          arguments."
                .into(),
            parameters: vec![],
        },
        ToolDefinition {
            name: ToolName::new(FAIL_TOOL),
            description: "The principle is violated. Provide a description of what's wrong and \
                          concrete guidance the agent should act on."
                .into(),
            parameters: vec![ToolParameter {
                name: "description".into(),
                r#type: "string".into(),
                description: "What the principle requires, how the call violates it, and the \
                              concrete change the agent should make."
                    .into(),
                required: true,
            }],
        },
    ]
}

fn gate_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: ToolName::new(GATE_APPLIES),
            description: "The condition applies to this situation; its sub-principles should be \
                          checked."
                .into(),
            parameters: vec![],
        },
        ToolDefinition {
            name: ToolName::new(GATE_SKIP),
            description: "The condition does not apply here. Skip this principle and its \
                          sub-principles."
                .into(),
            parameters: vec![],
        },
    ]
}

fn parse_verdict(call: &ToolCall) -> Result<Verdict> {
    match call.name.as_str() {
        PASS_TOOL => Ok(Verdict::Pass),
        FAIL_TOOL => {
            let description = call
                .arguments
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(Verdict::Fail(description))
        }
        other => Err(anyhow!("unknown verdict tool: {other}")),
    }
}
