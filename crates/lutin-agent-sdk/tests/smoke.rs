use lutin_agent_sdk::{
    Agent, AgentConfig, AgentEvent, FinishReason, LoopConfig, SamplingParams, ToolPolicy,
};
use futures::StreamExt;
use lutin_llm::mock::{MockProvider, MockResponse};

#[tokio::test]
async fn single_round_stop_on_no_tool_calls() {
    let provider = std::sync::Arc::new(MockProvider::new(vec![MockResponse::text("hello world")]));
    let config = AgentConfig {
        provider,
        model: lutin_llm::ModelId::from("mock-model"),
        sampling: SamplingParams::default(),
        system: "you are a test".into(),
        tool_policy: ToolPolicy::default(),
        loop_config: LoopConfig::default(),
    };
    let mut agent = Agent::new(config);
    agent.push_message(lutin_llm::Message::User("hi".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    let mut events = Vec::new();
    while let Some(evt) = stream.next().await {
        events.push(evt);
    }

    let outcome = agent.join().await;

    assert!(matches!(outcome.finish_reason, FinishReason::Stopped), "finish = {:?}", outcome.finish_reason);
    assert_eq!(outcome.rounds, 1);

    let has_assistant = events.iter().any(|e| matches!(e, AgentEvent::AssistantMessage(_)));
    assert!(has_assistant, "no AssistantMessage event");
    let finished = events.iter().any(|e| matches!(e, AgentEvent::Finished(_)));
    assert!(finished, "no Finished event");

    let last = outcome.last_assistant.expect("no last assistant");
    match last {
        lutin_llm::Message::Assistant { text, tool_calls, .. } => {
            assert_eq!(text, "hello world");
            assert!(tool_calls.is_empty());
        }
        _ => panic!("last_assistant wrong variant"),
    }
}
