use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lutin_agent_sdk::{
    Agent, AgentConfig, AgentEvent, DenyAll, FinishReason, LoopConfig,
    LoopDetection, SamplingParams, ToolError, ToolResult, ToolPolicy,
};
use futures::StreamExt;
use lutin_llm::mock::{MockProvider, MockResponse};
use lutin_llm::{LlmProvider, Usage};

mod support;
use support::{BadArgsProvider, EchoTool, TrapTool, UsageProvider};

fn cfg(provider: Arc<dyn LlmProvider>) -> AgentConfig {
    AgentConfig {
        provider,
        model: lutin_llm::ModelId::from("mock-model"),
        sampling: SamplingParams::default(),
        system: String::new(),
        tool_policy: ToolPolicy::default(),
        loop_config: LoopConfig::default(),
    }
}

#[tokio::test]
async fn mutator_returns_busy_while_running() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::always_text("hello"));
    let mut agent = Agent::new(cfg(Arc::clone(&provider)));
    agent.push_message(lutin_llm::Message::User("hi".into())).unwrap();

    let _stream = agent.start().expect("idle");
    let err = agent.update_config(|c| {
        c.provider = provider;
        c.model = lutin_llm::ModelId::from("other");
    });
    assert!(err.is_err(), "update_config must fail while running");

    let _ = agent.join().await;
}

#[tokio::test]
async fn double_start_errors() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::always_text("hi"));
    let mut agent = Agent::new(cfg(provider));
    agent.push_message(lutin_llm::Message::User("hi".into())).unwrap();

    let _stream = agent.start().expect("idle");
    let second = agent.start();
    assert!(second.is_err(), "second start must be AgentBusy");

    let _ = agent.join().await;
}

#[tokio::test]
async fn max_rounds_with_tool_calls() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![
        MockResponse::tool_call("c1", "echo", serde_json::json!({})),
    ]));
    let mut config = cfg(provider);
    config.loop_config.max_rounds = 1;
    let mut agent = Agent::new(config);
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.push_message(lutin_llm::Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    assert!(matches!(outcome.finish_reason, FinishReason::MaxRounds));
}

#[tokio::test]
async fn loop_detection_same_tool_call_repeated() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![
        MockResponse::tool_call("c1", "echo", serde_json::json!({"x":1})),
        MockResponse::tool_call("c2", "echo", serde_json::json!({"x":1})),
        MockResponse::tool_call("c3", "echo", serde_json::json!({"x":1})),
    ]));
    let mut config = cfg(provider);
    config.loop_config.loop_detection = LoopDetection::SameToolCallRepeated { threshold: 2 };
    config.loop_config.max_rounds = 5;
    let mut agent = Agent::new(config);
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.push_message(lutin_llm::Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    assert!(
        matches!(outcome.finish_reason, FinishReason::LoopDetected),
        "finish = {:?}",
        outcome.finish_reason
    );
}

#[tokio::test]
async fn approval_deny_emits_started_before_completed() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![
        MockResponse::tool_call("c1", "echo", serde_json::json!({})),
    ]));
    let mut agent = Agent::new(cfg(provider));
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.try_set_approval(Box::new(DenyAll)).unwrap();
    agent.push_message(lutin_llm::Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    let mut events = Vec::new();
    while let Some(e) = stream.next().await {
        events.push(e);
    }
    let _ = agent.join().await;

    let started_idx = events.iter().position(|e| matches!(e, AgentEvent::ToolCallArgsParsed(_)));
    let completed_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }));
    let s = started_idx.expect("no ToolCallStarted emitted");
    let c = completed_idx.expect("no ToolCallCompleted emitted");
    assert!(s < c, "started must precede completed");

    let mut found_denied_result = false;
    for e in &events {
        if let AgentEvent::ToolCallCompleted { outcome, .. } = e
            && let ToolResult::Err(ToolError::Denied(_)) = outcome
        {
            found_denied_result = true;
        }
    }
    assert!(found_denied_result, "no denied outcome in events");

    let last_msgs = agent.messages();
    let denied_msg = last_msgs.iter().find_map(|m| match m {
        lutin_llm::Message::ToolResult(tr) => Some(tr),
        _ => None,
    });
    let tr = denied_msg.expect("no tool result message");
    assert!(tr.is_error);
    assert!(tr.content.contains("denied"));
}

#[tokio::test]
async fn malformed_tool_args_yields_invalid_args() {
    let provider: Arc<dyn LlmProvider> = Arc::new(BadArgsProvider);
    let mut config = cfg(provider);
    config.loop_config.max_rounds = 1;
    let mut agent = Agent::new(config);
    agent.try_set_tools(support::toolbox_of(TrapTool)).unwrap();
    agent.push_message(lutin_llm::Message::User("hi".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    let mut outcomes: Vec<ToolResult> = Vec::new();
    while let Some(e) = stream.next().await {
        if let AgentEvent::ToolCallCompleted { outcome, .. } = e {
            outcomes.push(outcome);
        }
    }
    let _ = agent.join().await;

    assert_eq!(outcomes.len(), 1);
    assert!(matches!(outcomes[0], ToolResult::Err(ToolError::InvalidArgs(_))));
}

#[tokio::test]
async fn cancel_before_start_is_noop() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::always_text("hi"));
    let mut agent = Agent::new(cfg(provider));
    agent.cancel();
    assert!(!agent.is_running());
}

#[tokio::test]
async fn cancel_after_done_is_noop() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![MockResponse::text("hi")]));
    let mut agent = Agent::new(cfg(provider));
    agent.push_message(lutin_llm::Message::User("hi".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let _ = agent.join().await;

    agent.cancel();
    assert!(!agent.is_running());
}

#[tokio::test]
async fn token_usage_summed_across_rounds() {
    let provider = UsageProvider {
        responses: Mutex::new(VecDeque::from(vec![
            (
                String::new(),
                true,
                Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 0,
                },
            ),
            (
                "done".into(),
                false,
                Usage {
                    prompt_tokens: 20,
                    completion_tokens: 8,
                    total_tokens: 28,
                },
            ),
        ])),
    };

    let provider: Arc<dyn LlmProvider> = Arc::new(provider);
    let mut agent = Agent::new(cfg(provider));
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.push_message(lutin_llm::Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    assert_eq!(outcome.usage.prompt_tokens, 30);
    assert_eq!(outcome.usage.completion_tokens, 13);
    assert_eq!(outcome.usage.total_tokens, 43);
}
