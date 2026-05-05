//! Tests for Phase A extensions: pre-round hook, richer stop conditions,
//! and stream inactivity timeout.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lutin_agent_sdk::{
    Agent, AgentConfig, AgentError, AgentEvent, DenyAll, FinishReason, LoopConfig,
    PreRoundOutput, RoundSummary, SamplingParams, StopCondition, ToolCallRecord, ToolPolicy,
};
use async_trait::async_trait;
use futures::StreamExt;
use lutin_llm::mock::{MockProvider, MockResponse};
use lutin_llm::{
    CompletionRequest, CompletionResponse, EventStream, LlmError, LlmProvider, Message,
    ModelInfo, StreamEvent,
};

mod support;
use support::EchoTool;

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

/// Provider that records every request it receives.
struct CapturingProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
    responses: Mutex<std::collections::VecDeque<MockResponse>>,
}

impl CapturingProvider {
    fn new(captured: Arc<Mutex<Vec<CompletionRequest>>>, responses: Vec<MockResponse>) -> Self {
        Self {
            captured,
            responses: Mutex::new(responses.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for CapturingProvider {
    async fn complete(&self, _r: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        unreachable!()
    }
    async fn stream(&self, r: CompletionRequest) -> Result<EventStream, LlmError> {
        self.captured.lock().unwrap().push(r);
        let resp = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| MockResponse::text("fallback"));
        let events: Vec<Result<StreamEvent, LlmError>> =
            resp.to_events_public().into_iter().collect();
        Ok(Box::pin(futures::stream::iter(events)))
    }
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![])
    }
}

// Workaround: re-implement the to_events surface (the trait method is pub(crate)
// in lutin_llm::mock). Build the stream manually from a MockResponse.
trait MockResponseExt {
    fn to_events_public(&self) -> Vec<Result<StreamEvent, LlmError>>;
}

impl MockResponseExt for MockResponse {
    fn to_events_public(&self) -> Vec<Result<StreamEvent, LlmError>> {
        let mut events: Vec<Result<StreamEvent, LlmError>> = Vec::new();
        if !self.text.is_empty() {
            events.push(Ok(StreamEvent::Delta(self.text.clone())));
        }
        for tc in &self.tool_calls {
            events.push(Ok(StreamEvent::ToolCallStart {
                id: tc.id.clone(),
                name: tc.name.clone(),
            }));
            let args_str = serde_json::to_string(&tc.arguments).unwrap_or_default();
            events.push(Ok(StreamEvent::ToolCallDelta {
                id: tc.id.clone(),
                arguments: args_str,
            }));
        }
        events.push(Ok(StreamEvent::Done { usage: None }));
        events
    }
}

#[tokio::test]
async fn pre_round_hook_injects_user_message_into_next_request() {
    let captured = Arc::new(Mutex::new(Vec::<CompletionRequest>::new()));
    let provider: Arc<dyn LlmProvider> = Arc::new(CapturingProvider::new(
        Arc::clone(&captured),
        vec![MockResponse::text("done")],
    ));

    let mut config = cfg(provider);
    let injected = Arc::new(Mutex::new(0u32));
    let injected_ref = Arc::clone(&injected);
    config.loop_config.pre_round = Some(Arc::new(move |round: u32| {
        let r = Arc::clone(&injected_ref);
        Box::pin(async move {
            *r.lock().unwrap() += 1;
            PreRoundOutput::with_messages(vec![Message::User(format!("injected@{round}"))])
        })
    }));

    let mut agent = Agent::new(config);
    agent.push_message(Message::User("start".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;
    assert!(matches!(outcome.finish_reason, FinishReason::Stopped));
    assert_eq!(*injected.lock().unwrap(), 1);

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let last = &reqs[0];
    let found = last.messages.iter().any(|m| matches!(m, Message::User(t) if t == "injected@1"));
    assert!(found, "injected message did not reach provider: {:?}", last.messages);
}

#[tokio::test]
async fn stop_on_tool_called_fires_by_name() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![MockResponse::tool_call(
        "c1",
        "echo",
        serde_json::json!({}),
    )]));
    let mut config = cfg(provider);
    config.loop_config.max_rounds = 5;
    config.loop_config.stop_condition = StopCondition::ToolCalled(lutin_llm::ToolName::new("echo"));

    let mut agent = Agent::new(config);
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.push_message(Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    assert!(
        matches!(outcome.finish_reason, FinishReason::Stopped),
        "finish = {:?}",
        outcome.finish_reason
    );
    assert_eq!(outcome.rounds, 1);
}

#[tokio::test]
async fn stop_on_any_call_denied() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![MockResponse::tool_call(
        "c1",
        "echo",
        serde_json::json!({}),
    )]));
    let mut config = cfg(provider);
    config.loop_config.max_rounds = 5;
    config.loop_config.stop_condition = StopCondition::AnyCallDenied;

    let mut agent = Agent::new(config);
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.try_set_approval(Box::new(DenyAll)).unwrap();
    agent.push_message(Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    assert!(
        matches!(outcome.finish_reason, FinishReason::Stopped),
        "finish = {:?}",
        outcome.finish_reason
    );
    assert_eq!(outcome.rounds, 1);
}

#[tokio::test]
async fn custom_stop_sees_records_and_denied_count() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![MockResponse::tool_call(
        "c1",
        "echo",
        serde_json::json!({}),
    )]));
    let mut config = cfg(provider);
    config.loop_config.max_rounds = 5;
    let seen_name = Arc::new(Mutex::new(String::new()));
    let seen_denied = Arc::new(Mutex::new(0u32));
    let n = Arc::clone(&seen_name);
    let d = Arc::clone(&seen_denied);
    config.loop_config.stop_condition = StopCondition::Custom(Arc::new(
        move |summary: &RoundSummary, records: &[ToolCallRecord]| {
            if let Some(first) = records.first() {
                *n.lock().unwrap() = first.name.as_str().to_string();
            }
            *d.lock().unwrap() = summary.denied_count;
            true
        },
    ));

    let mut agent = Agent::new(config);
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.try_set_approval(Box::new(DenyAll)).unwrap();
    agent.push_message(Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    assert!(matches!(outcome.finish_reason, FinishReason::Stopped));
    assert_eq!(&*seen_name.lock().unwrap(), "echo");
    assert_eq!(*seen_denied.lock().unwrap(), 1);
}

/// Provider that emits one delta then never completes the stream — triggers
/// the inactivity timeout.
struct StalledProvider;

#[async_trait]
impl LlmProvider for StalledProvider {
    async fn complete(&self, _r: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        unreachable!()
    }
    async fn stream(&self, _r: CompletionRequest) -> Result<EventStream, LlmError> {
        // Emit one delta immediately, then a future that never resolves.
        let first = futures::stream::iter(vec![Ok::<_, LlmError>(StreamEvent::Delta(
            "partial".into(),
        ))]);
        let pending = futures::stream::poll_fn(|_cx| std::task::Poll::Pending);
        Ok(Box::pin(first.chain(pending)))
    }
    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn pre_round_panic_surfaces_as_terminal_error() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![MockResponse::text("never seen")]));
    let mut config = cfg(provider);
    config.loop_config.pre_round = Some(Arc::new(|_round: u32| {
        Box::pin(async move {
            panic!("boom in pre_round");
            #[allow(unreachable_code)]
            PreRoundOutput::default()
        })
    }));

    let mut agent = Agent::new(config);
    agent.push_message(Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    let mut saw_error = false;
    let mut terminal: Option<FinishReason> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Error(_) => saw_error = true,
            AgentEvent::Finished(r) => terminal = Some(r),
            _ => {}
        }
    }
    assert!(saw_error, "expected AgentEvent::Error before Finished");
    assert!(
        matches!(terminal, Some(FinishReason::Error(_))),
        "terminal = {terminal:?}",
    );

    let outcome = agent.join().await;
    let err = match &outcome.finish_reason {
        FinishReason::Error(e) => e.clone(),
        other => panic!("expected FinishReason::Error, got {other:?}"),
    };
    assert!(
        matches!(&*err, AgentError::Internal(m) if m.contains("pre_round hook panicked")),
        "unexpected err: {err:?}",
    );
}

#[tokio::test]
async fn pre_round_system_override_reaches_provider() {
    let captured = Arc::new(Mutex::new(Vec::<CompletionRequest>::new()));
    let provider: Arc<dyn LlmProvider> = Arc::new(CapturingProvider::new(
        Arc::clone(&captured),
        vec![MockResponse::text("done")],
    ));

    let mut config = cfg(provider);
    config.system = "original-sys".into();
    config.loop_config.pre_round = Some(Arc::new(|_round: u32| {
        Box::pin(async move {
            let mut out = PreRoundOutput::default();
            out.system = Some("fresh-sys".into());
            out
        })
    }));

    let mut agent = Agent::new(config);
    agent.push_message(Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let _ = agent.join().await;

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let first = reqs[0].messages.first().expect("at least one message");
    match first {
        Message::System(s) => assert_eq!(s, "fresh-sys"),
        other => panic!("expected System(fresh-sys), got {other:?}"),
    }
}

#[tokio::test]
async fn tool_called_stop_ignores_denied_calls() {
    // Loop over 2 rounds: round 1 emits a `foo` call which is denied; the
    // `ToolCalled("foo")` stop must NOT fire. Round 2 emits plain text with
    // no tool → loop ends via MaxRounds.
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![
        MockResponse::tool_call("c1", "foo", serde_json::json!({})),
        MockResponse::text("no tool this time"),
    ]));
    let mut config = cfg(provider);
    config.loop_config.max_rounds = 2;
    config.loop_config.stop_condition = StopCondition::ToolCalled(lutin_llm::ToolName::new("foo"));

    let mut agent = Agent::new(config);
    agent.try_set_tools(support::toolbox_of(EchoTool)).unwrap();
    agent.try_set_approval(Box::new(DenyAll)).unwrap();
    agent.push_message(Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    // Denied call in round 1 must NOT stop the loop; we should reach round 2
    // and then terminate at max_rounds.
    assert_eq!(outcome.rounds, 2, "expected two rounds, got {}", outcome.rounds);
    assert!(
        matches!(outcome.finish_reason, FinishReason::MaxRounds),
        "finish = {:?}",
        outcome.finish_reason,
    );
}

#[tokio::test]
async fn assistant_text_emitted_per_delta_no_coalescing() {
    // Provider that emits N distinct Delta chunks.
    struct SplitProvider {
        chunks: Vec<String>,
    }
    #[async_trait]
    impl LlmProvider for SplitProvider {
        async fn complete(&self, _r: CompletionRequest) -> Result<CompletionResponse, LlmError> {
            unreachable!()
        }
        async fn stream(&self, _r: CompletionRequest) -> Result<EventStream, LlmError> {
            let mut evs: Vec<Result<StreamEvent, LlmError>> = self
                .chunks
                .iter()
                .map(|c| Ok(StreamEvent::Delta(c.clone())))
                .collect();
            evs.push(Ok(StreamEvent::Done { usage: None }));
            Ok(Box::pin(futures::stream::iter(evs)))
        }
        async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
            Ok(vec![])
        }
    }

    let chunks: Vec<String> = vec!["a".into(), "bc".into(), "d".into(), "efgh".into()];
    let provider: Arc<dyn LlmProvider> = Arc::new(SplitProvider { chunks: chunks.clone() });
    let mut agent = Agent::new(cfg(provider));
    agent.push_message(Message::User("go".into())).unwrap();

    let mut stream = agent.start().expect("idle");
    let mut texts: Vec<String> = Vec::new();
    while let Some(ev) = stream.next().await {
        if let AgentEvent::AssistantText(t) = ev {
            texts.push(t);
        }
    }
    let _ = agent.join().await;

    assert_eq!(texts, chunks, "per-delta AssistantText contract violated");
}

#[tokio::test]
async fn stream_inactivity_timeout_fires() {
    let provider: Arc<dyn LlmProvider> = Arc::new(StalledProvider);
    let mut config = cfg(provider);
    config.loop_config.max_rounds = 2;
    let timeout = Duration::from_millis(100);
    config.loop_config.stream_inactivity_timeout = Some(timeout);

    let mut agent = Agent::new(config);
    agent.push_message(Message::User("hi".into())).unwrap();

    let started = std::time::Instant::now();
    let mut stream = agent.start().expect("idle");
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;
    let elapsed = started.elapsed();
    // Bound: should fire close to `timeout`, well under 5s.
    assert!(elapsed < Duration::from_secs(5), "elapsed = {elapsed:?}");

    let err = match &outcome.finish_reason {
        FinishReason::Error(e) => e.clone(),
        other => panic!("finish = {other:?}"),
    };
    assert!(
        matches!(&*err, AgentError::StreamStalled(_)),
        "unexpected error: {err:?}"
    );
}
