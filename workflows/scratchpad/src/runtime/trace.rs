use lutin_llm::{CompletionResponse, Message};
use tracing::info;

pub(crate) fn log_messages(stage: &'static str, messages: &[Message]) {
    info!(stage, count = messages.len(), "main agent: messages -->");
    for (i, m) in messages.iter().enumerate() {
        let (role, body) = render(m);
        info!(stage, idx = i, role, "{body}");
    }
}

pub(crate) fn log_response(stage: &'static str, attempt: usize, response: &CompletionResponse) {
    let calls: Vec<String> = response
        .tool_calls
        .iter()
        .map(|tc| {
            let args = serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "<?>".into());
            format!("{}({args})", tc.name.as_str())
        })
        .collect();
    info!(
        stage,
        attempt,
        text = %response.text,
        thinking = ?response.thinking,
        tool_calls = ?calls,
        "main agent: response <--"
    );
}

fn render(m: &Message) -> (&'static str, String) {
    match m {
        Message::System(s) => ("system", s.clone()),
        Message::User(s) => ("user", s.clone()),
        Message::Assistant { text, tool_calls, thinking } => {
            let mut s = String::new();
            if let Some(t) = thinking
                && !t.is_empty()
            {
                s.push_str(&format!("[thinking] {t}\n"));
            }
            if !text.is_empty() {
                s.push_str(text);
                s.push('\n');
            }
            for tc in tool_calls {
                let args = serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "<?>".into());
                s.push_str(&format!("-> {}({args})\n", tc.name.as_str()));
            }
            ("assistant", s)
        }
        Message::ToolResult(tr) => (
            "tool",
            format!("[id={}] {}", tr.call_id.as_str(), tr.content),
        ),
        Message::Image { items } => ("image", format!("{} items", items.len())),
        Message::SubAgentReply { agent_id, text } => ("subagent_reply", format!("[{agent_id}] {text}")),
        Message::SubAgentFailure { agent_id, reason } => {
            ("subagent_fail", format!("[{agent_id}] {reason}"))
        }
        Message::Summary { text } => ("summary", text.clone()),
    }
}
