//! Per-round transcript projection. The driver applies a projector
//! before each `CompletionRequest` is built, so the slice the model
//! sees can be narrower than the agent's owned history without losing
//! any messages from the agent's `Vec<Message>`.

use std::sync::Arc;

use lutin_llm::Message;

/// Maps the agent's full transcript to the slice the next round will
/// send to the provider. Called once per round, after any `pre_round`
/// hook injection has been applied.
pub type MessageProjector = Arc<dyn Fn(&[Message]) -> Vec<Message> + Send + Sync>;

/// Keep only the last `n` user turns (plus the assistant rounds and
/// tool exchanges that follow each kept user turn). Any leading
/// `Message::System(_)` entries are preserved at the front so callers
/// who embed their own system message don't lose it. When the
/// transcript has fewer than `n` user turns, the full slice is cloned
/// through unchanged.
///
/// `n == 0` returns just the leading System messages (drops every
/// conversational turn — degenerate but well-defined).
pub fn slide_window_by_user_turns(messages: &[Message], n: usize) -> Vec<Message> {
    let leading_system_end = messages
        .iter()
        .position(|m| !matches!(m, Message::System(_)))
        .unwrap_or(messages.len());
    let leading = &messages[..leading_system_end];
    let body = &messages[leading_system_end..];

    if n == 0 {
        return leading.to_vec();
    }

    let mut count = 0usize;
    let mut keep_from: Option<usize> = None;
    for (i, m) in body.iter().enumerate().rev() {
        if matches!(m, Message::User(_)) {
            count += 1;
            if count == n {
                keep_from = Some(i);
                break;
            }
        }
    }

    let body_view: &[Message] = match keep_from {
        Some(i) => &body[i..],
        None => body,
    };

    let mut out = Vec::with_capacity(leading.len() + body_view.len());
    out.extend(leading.iter().cloned());
    out.extend(body_view.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lutin_llm::Message;

    fn user(s: &str) -> Message {
        Message::User(s.into())
    }
    fn assistant(s: &str) -> Message {
        Message::Assistant {
            text: s.into(),
            tool_calls: Vec::new(),
            thinking: None,
        }
    }
    fn system(s: &str) -> Message {
        Message::System(s.into())
    }
    fn user_text(m: &Message) -> Option<&str> {
        match m {
            Message::User(t) => Some(t.as_str()),
            _ => None,
        }
    }

    #[test]
    fn keeps_full_history_when_under_n() {
        let msgs = vec![user("a"), assistant("A"), user("b"), assistant("B")];
        let out = slide_window_by_user_turns(&msgs, 5);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn keeps_last_n_user_turns_with_following_assistant_rounds() {
        let msgs = vec![
            user("u1"),
            assistant("a1"),
            user("u2"),
            assistant("a2"),
            user("u3"),
            assistant("a3"),
        ];
        let out = slide_window_by_user_turns(&msgs, 2);
        // u1/a1 dropped; u2 onwards kept.
        assert_eq!(out.len(), 4);
        assert_eq!(user_text(&out[0]), Some("u2"));
    }

    #[test]
    fn preserves_leading_system_messages() {
        let msgs = vec![
            system("sys"),
            user("u1"),
            assistant("a1"),
            user("u2"),
            assistant("a2"),
        ];
        let out = slide_window_by_user_turns(&msgs, 1);
        assert!(matches!(out[0], Message::System(_)));
        assert_eq!(user_text(&out[1]), Some("u2"));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn n_zero_drops_all_turns_keeps_system() {
        let msgs = vec![system("sys"), user("u1"), assistant("a1")];
        let out = slide_window_by_user_turns(&msgs, 0);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Message::System(_)));
    }

    #[test]
    fn keeps_tool_exchanges_within_window() {
        // A user turn that triggers a tool call must keep the
        // assistant{tool_calls} + ToolResult pair together.
        let msgs = vec![
            user("u1"),
            assistant("a1"),
            user("u2"),
            Message::Assistant {
                text: String::new(),
                tool_calls: vec![lutin_llm::ToolCall {
                    id: lutin_llm::CallId::from("c1"),
                    name: lutin_llm::ToolName::from("t"),
                    arguments: serde_json::Value::Null,
                }],
                thinking: None,
            },
            Message::ToolResult(lutin_llm::ToolResultContent {
                call_id: lutin_llm::CallId::from("c1"),
                content: "ok".into(),
                is_error: false,
            }),
            assistant("a2"),
        ];
        let out = slide_window_by_user_turns(&msgs, 1);
        assert_eq!(user_text(&out[0]), Some("u2"));
        // Assistant{tool_calls}, ToolResult, Assistant text — all retained.
        assert_eq!(out.len(), 4);
    }
}
