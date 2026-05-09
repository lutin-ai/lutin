//! One-shot reviewer LLM call for a single principle.
//!
//! Takes a `Principle`, a proposed `ToolCall`, and a bag of optional
//! context (artifact preview, chat transcript, prior steps), formats a
//! prompt that asks the reviewer to judge *just that one principle*,
//! and parses the response back into a `Verdict`.
//!
//! This module owns prompt construction + JSON parsing only. Provider
//! selection, scheduling, and what to *do* with the returned verdict
//! all live in the review-loop driver one layer up.

use lutin_entities::Persona;
use lutin_llm::{
    CompletionRequest, LlmError, LlmProvider, Message as LlmMessage, ModelId, ToolCall,
    ToolDefinition, ToolName, ToolParameter,
};
use serde::Deserialize;

/// Name the reviewer LLM uses to submit its verdict. Stable string —
/// matches what we register in the tool definition and what we look
/// for in `response.tool_calls`.
const VERDICT_TOOL: &str = "submit_verdict";

use crate::principle::{ContextItem, Principle};
use crate::step::{BlockingSeverity, Verdict, VerdictKind};

/// Inputs assembled by the driver. Each field tracks one of the
/// `ContextItem` variants the principle opted into. Driver passes
/// `None` for any item the principle didn't request.
pub struct ReviewInputs<'a> {
    pub principle: &'a Principle,
    pub call: &'a ToolCall,
    /// Pre-execution artifact when the tool kind permits one
    /// (currently Edit/Write → simulated post-edit file content).
    pub artifact: Option<&'a str>,
    /// Main user-agent transcript so far. Already projected to plain
    /// text to keep the reviewer prompt provider-agnostic.
    pub chat: Option<&'a str>,
    /// Accepted prior step frames, projected to text.
    pub prior_steps: Option<&'a str>,
}

/// Run the reviewer call. Persona resolution + provider construction
/// are the caller's responsibility — this function takes the
/// already-built provider and model id so it stays cheap to unit-test
/// against `MockProvider`.
pub async fn review(
    provider: &dyn LlmProvider,
    model: &ModelId,
    persona: &Persona,
    inputs: ReviewInputs<'_>,
) -> Result<Verdict, ReviewerError> {
    let system = build_system_prompt(persona, inputs.principle);
    let user = build_user_prompt(&inputs);
    let request = CompletionRequest {
        model: model.clone(),
        messages: vec![LlmMessage::System(system), LlmMessage::User(user)],
        // Hardcoded reviewer-only tool. Never registered in the agent
        // SDK toolbox — exists solely on this CompletionRequest so the
        // reviewer model emits its verdict as structured args instead
        // of JSON in `content`. Keeps reasoning models from leaking
        // chain-of-thought into the parser.
        tools: vec![submit_verdict_tool()],
        temperature: persona.temperature.or(Some(0.0)),
        presence_penalty: persona.presence_penalty,
        // 10k = ~100x normal verdict size. Generous headroom in case
        // a model leaks any reasoning before the tool call, but still
        // bounded so a misbehaving reviewer can't burn through the
        // whole context window. Hitting the cap returns
        // `finish_reason=length` cleanly — no crash, just a parse
        // failure that propagates as a review system failure.
        max_tokens: Some(10240),
        thinking_enabled: false,
        extensions: Default::default(),
    };
    let response = provider.complete(request).await.map_err(ReviewerError::Llm)?;
    // Prefer the tool call. Models that honor the tool emit one
    // `submit_verdict` call with the verdict in args; we parse those
    // directly. Providers that ignore tools (or models that do their
    // own thing) fall through to the lenient text parser, which
    // handles raw JSON, code-fenced JSON, and JSON buried in a
    // chain-of-thought monologue.
    if let Some(call) = response.tool_calls.first() {
        if call.name.as_str() == VERDICT_TOOL {
            return parse_verdict_from_args(&inputs.principle.name, &call.arguments);
        }
    }
    parse_verdict(&inputs.principle.name, &response.text)
}

/// Tool the reviewer LLM is asked to call. Args mirror `RawVerdict`.
/// `ToolParameter` doesn't currently encode `enum` constraints — the
/// allowed values live in each parameter's description and are
/// validated by `parse_verdict_from_args` after the call lands.
fn submit_verdict_tool() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new(VERDICT_TOOL),
        description:
            "Submit your verdict on the proposed tool call against the assigned principle. \
             Call this exactly once."
                .into(),
        parameters: vec![
            ToolParameter {
                name: "verdict".into(),
                r#type: "string".into(),
                description:
                    "Either \"pass\" (the proposed step satisfies the principle) or \"fail\" \
                     (it violates the principle)."
                        .into(),
                required: true,
            },
            ToolParameter {
                name: "severity".into(),
                r#type: "string".into(),
                description:
                    "Required when verdict=\"fail\". Use \"fix\" when the agent should retry \
                     this step, \"rethink\" when the step's premise is wrong and the loop \
                     should rewind further. With verdict=\"pass\", set to \"nit\" for a \
                     pass-with-note or omit otherwise."
                        .into(),
                required: false,
            },
            ToolParameter {
                name: "reasoning".into(),
                r#type: "string".into(),
                description: "Short justification (1-3 sentences) for your verdict.".into(),
                required: false,
            },
            ToolParameter {
                name: "suggested_fix".into(),
                r#type: "string".into(),
                description:
                    "Optional concrete change the agent could make to satisfy the principle. \
                     Leave unset when verdict=\"pass\"."
                        .into(),
                required: false,
            },
        ],
    }
}

fn parse_verdict_from_args(
    principle_name: &str,
    arguments: &serde_json::Value,
) -> Result<Verdict, ReviewerError> {
    let v: RawVerdict = serde_json::from_value(arguments.clone())
        .map_err(|e| ReviewerError::Parse(format!("tool args: {e}: {arguments}")))?;
    raw_to_verdict(principle_name, v)
}

fn build_system_prompt(persona: &Persona, principle: &Principle) -> String {
    let mut s = String::new();
    if !persona.system_prompt.is_empty() {
        s.push_str(&persona.system_prompt);
        s.push_str("\n\n");
    }
    // Verdict shape lives on the `submit_verdict` tool definition,
    // not in the system prompt — repeating it here just gives
    // reasoning models another excuse to monologue about JSON.
    s.push_str(
        "You are a single-principle reviewer. Judge ONLY the principle below; ignore every \
         other concern. Submit your verdict by calling the `submit_verdict` tool exactly once.\n\n",
    );
    s.push_str("PRINCIPLE: ");
    s.push_str(&principle.title);
    s.push_str("\n\n");
    s.push_str(&principle.description);
    s
}

fn build_user_prompt(inputs: &ReviewInputs<'_>) -> String {
    let mut s = String::new();
    s.push_str("Proposed tool call:\n");
    s.push_str("  name: ");
    s.push_str(inputs.call.name.as_str());
    s.push_str("\n  arguments: ");
    let args = serde_json::to_string(&inputs.call.arguments).unwrap_or_else(|_| "<unserializable>".into());
    s.push_str(&args);
    s.push('\n');

    let wants = |c: ContextItem| inputs.principle.context.contains(&c);

    if wants(ContextItem::ToolArtifact) {
        if let Some(a) = inputs.artifact {
            s.push_str("\nPost-execution artifact (simulated, not yet committed):\n");
            s.push_str(a);
            if !a.ends_with('\n') {
                s.push('\n');
            }
        }
    }
    if wants(ContextItem::Chat) {
        if let Some(c) = inputs.chat {
            s.push_str("\nConversation so far:\n");
            s.push_str(c);
            if !c.ends_with('\n') {
                s.push('\n');
            }
        }
    }
    if wants(ContextItem::PriorSteps) {
        if let Some(p) = inputs.prior_steps {
            s.push_str("\nPrior accepted steps:\n");
            s.push_str(p);
            if !p.ends_with('\n') {
                s.push('\n');
            }
        }
    }

    s.push_str("\nCall `submit_verdict` with your verdict now.");
    s
}

#[derive(Debug, Deserialize)]
struct RawVerdict {
    verdict: String,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    suggested_fix: Option<String>,
}

fn parse_verdict(principle_name: &str, raw: &str) -> Result<Verdict, ReviewerError> {
    // Fallback path for providers/models that ignore the
    // `submit_verdict` tool and instead emit the verdict in `content`.
    // Reasoning models (Qwen, DeepSeek, GPT-OSS, etc.) frequently dump
    // their full chain-of-thought before the JSON verdict — and
    // sometimes emit the same JSON twice. We pick the LAST balanced
    // `{…}` substring that parses as `RawVerdict`, which lands on the
    // model's final answer regardless of any think-tags, prose, or
    // code fences around it. Falling back to strict parse on the
    // whole string keeps the happy path fast.
    let trimmed = strip_fences(raw.trim());
    let v: RawVerdict = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => extract_last_verdict_object(raw)
            .ok_or_else(|| ReviewerError::Parse(format!("no JSON verdict found in: {raw}")))?,
    };
    raw_to_verdict(principle_name, v)
}

fn raw_to_verdict(principle_name: &str, v: RawVerdict) -> Result<Verdict, ReviewerError> {
    let kind = match (v.verdict.as_str(), v.severity.as_deref()) {
        ("pass", None) => VerdictKind::Pass,
        ("pass", Some("nit")) => VerdictKind::PassWithNit {
            reasoning: v.reasoning,
        },
        ("pass", Some(other)) => {
            return Err(ReviewerError::Parse(format!(
                "pass with non-nit severity={other}"
            )));
        }
        // Fail without an explicit severity defaults to "fix" so the
        // agent retries rather than rewinding — narrower default than
        // forcing models to always emit a severity.
        ("fail", sev) => {
            let severity = match sev {
                None | Some("fix") => BlockingSeverity::Fix,
                Some("rethink") => BlockingSeverity::Rethink,
                Some("nit") => {
                    return Err(ReviewerError::Parse(
                        "fail with severity=nit is contradictory".into(),
                    ));
                }
                Some(other) => {
                    return Err(ReviewerError::Parse(format!("severity={other}")));
                }
            };
            VerdictKind::Fail {
                severity,
                reasoning: v.reasoning,
                suggested_fix: v.suggested_fix,
            }
        }
        (other, _) => return Err(ReviewerError::Parse(format!("verdict={other}"))),
    };
    Ok(Verdict {
        principle_name: principle_name.to_string(),
        kind,
    })
}

/// Walk `raw` and return the last balanced `{…}` substring that
/// successfully parses as a `RawVerdict`. Quote-aware so strings
/// containing `{` or `}` don't throw off the brace count. We scan
/// right-to-left over the candidates because models often emit the
/// JSON twice (once inside the chain-of-thought, once as the final
/// answer) — the trailing one is the canonical verdict.
fn extract_last_verdict_object(raw: &str) -> Option<RawVerdict> {
    let bytes = raw.as_bytes();
    let mut candidates: Vec<(usize, usize)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => stack.push(i),
            b'}' => {
                if let Some(start) = stack.pop() {
                    // Only top-level objects matter — nested ones
                    // are part of an enclosing candidate's body and
                    // would be tried via that candidate.
                    if stack.is_empty() {
                        candidates.push((start, i + 1));
                    }
                }
            }
            _ => {}
        }
    }
    for (start, end) in candidates.into_iter().rev() {
        let slice = &raw[start..end];
        if let Ok(v) = serde_json::from_str::<RawVerdict>(slice) {
            return Some(v);
        }
    }
    None
}

/// Strip ``` fences if the model wrapped its JSON in them despite
/// being told not to. Tolerant: works with or without language tag.
fn strip_fences(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        let rest = rest.trim_start_matches(|c: char| c.is_alphanumeric());
        let rest = rest.trim_start_matches('\n');
        if let Some(end) = rest.rfind("```") {
            return rest[..end].trim();
        }
    }
    s
}

#[derive(Debug, thiserror::Error)]
pub enum ReviewerError {
    #[error("llm: {0}")]
    Llm(#[source] LlmError),
    #[error("parse verdict: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pass() {
        let v = parse_verdict(
            "p",
            r#"{"verdict":"pass","reasoning":"looks fine"}"#,
        )
        .unwrap();
        assert_eq!(v.principle_name, "p");
        assert!(matches!(v.kind, VerdictKind::Pass));
    }

    #[test]
    fn parse_pass_with_nit() {
        let v = parse_verdict(
            "p",
            r#"{"verdict":"pass","severity":"nit","reasoning":"meh"}"#,
        )
        .unwrap();
        assert!(matches!(v.kind, VerdictKind::PassWithNit { .. }));
    }

    #[test]
    fn parse_fail_defaults_to_fix() {
        let v = parse_verdict(
            "p",
            r#"{"verdict":"fail","reasoning":"nope"}"#,
        )
        .unwrap();
        assert!(matches!(
            v.kind,
            VerdictKind::Fail { severity: BlockingSeverity::Fix, .. }
        ));
    }

    #[test]
    fn parse_rethink() {
        let v = parse_verdict(
            "p",
            r#"{"verdict":"fail","severity":"rethink","reasoning":"wrong premise"}"#,
        )
        .unwrap();
        assert!(matches!(
            v.kind,
            VerdictKind::Fail { severity: BlockingSeverity::Rethink, .. }
        ));
    }

    #[test]
    fn parse_strips_code_fences() {
        let v = parse_verdict(
            "p",
            "```json\n{\"verdict\":\"pass\",\"reasoning\":\"x\"}\n```",
        )
        .unwrap();
        assert!(matches!(v.kind, VerdictKind::Pass));
    }

    #[test]
    fn parse_invalid_returns_err() {
        let r = parse_verdict("p", "not json");
        assert!(matches!(r, Err(ReviewerError::Parse(_))));
    }

    #[test]
    fn parse_extracts_json_after_thinking_trace() {
        // Reasoning models often emit a long prose monologue before
        // the JSON. The extractor should land on the trailing JSON
        // even when the leading text is several paragraphs.
        let raw = r#"
The user wants me to evaluate the proposed edit. Let me think.
The principle is "delete dead code". The edit adds a closure, no
dead code is involved. So verdict is pass.

{"verdict":"pass","reasoning":"adds a closure; not about dead code"}
"#;
        let v = parse_verdict("p", raw).unwrap();
        assert!(matches!(v.kind, VerdictKind::Pass));
    }

    #[test]
    fn parse_picks_last_when_json_appears_twice() {
        // Models sometimes emit a tentative JSON inside the thought
        // trace and then a final corrected JSON. Take the trailing one.
        let raw = r#"
First draft: {"verdict":"fail","severity":"fix","reasoning":"wrong"}
On reflection that was wrong. Final answer:
{"verdict":"pass","reasoning":"actually fine"}
"#;
        let v = parse_verdict("p", raw).unwrap();
        assert!(matches!(v.kind, VerdictKind::Pass));
    }

    #[test]
    fn parse_args_tool_path_pass() {
        let args = serde_json::json!({
            "verdict": "pass",
            "reasoning": "looks fine",
        });
        let v = parse_verdict_from_args("p", &args).unwrap();
        assert!(matches!(v.kind, VerdictKind::Pass));
    }

    #[test]
    fn parse_args_tool_path_fail_rethink() {
        let args = serde_json::json!({
            "verdict": "fail",
            "severity": "rethink",
            "reasoning": "wrong premise",
        });
        let v = parse_verdict_from_args("p", &args).unwrap();
        assert!(matches!(
            v.kind,
            VerdictKind::Fail { severity: BlockingSeverity::Rethink, .. }
        ));
    }

    #[test]
    fn parse_args_tool_path_rejects_pass_with_fix() {
        // Same invariant as the text path: pass+fix is contradictory.
        let args = serde_json::json!({
            "verdict": "pass",
            "severity": "fix",
            "reasoning": "x",
        });
        let r = parse_verdict_from_args("p", &args);
        assert!(matches!(r, Err(ReviewerError::Parse(_))));
    }

    #[test]
    fn submit_verdict_tool_shape_is_stable() {
        // The hardcoded reviewer tool name + parameter set is what
        // `review()` looks for in `response.tool_calls`. A rename
        // here without updating `VERDICT_TOOL` would silently fall
        // back to the text parser on every reviewer call.
        let t = submit_verdict_tool();
        assert_eq!(t.name.as_str(), VERDICT_TOOL);
        let names: Vec<&str> = t.parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["verdict", "severity", "reasoning", "suggested_fix"]);
    }

    #[test]
    fn parse_pass_with_fix_severity_is_rejected() {
        // Used to be representable in the old `Verdict { passed:true,
        // severity:Some(Fix) }` shape — now caught at the parser.
        let r = parse_verdict(
            "p",
            r#"{"verdict":"pass","severity":"fix","reasoning":"x"}"#,
        );
        assert!(matches!(r, Err(ReviewerError::Parse(_))));
    }
}
