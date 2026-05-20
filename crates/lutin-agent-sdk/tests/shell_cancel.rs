//! End-to-end agent cancellation test for the Shell tool.
//!
//! Wires a real `Shell` tool into a real `Agent` driven by a `MockProvider`.
//! The mock emits a single `shell` tool call whose bash command writes its
//! own PID and then sleeps for far longer than the test will live. We wait
//! until bash has written its PID (proving the tool dispatched and the
//! child is running), then fire `agent.cancel()` — the same call the chat
//! engine makes when the user hits Stop — and assert two things:
//!
//!   1. The run finishes with `FinishReason::Cancelled`.
//!   2. The bash PID is dead within a few seconds.
//!
//! If `kill_on_drop` regresses, or the run-loop stops dropping the in-flight
//! `run_round` future on cancel, (2) fails and we know cancel no longer
//! reaches OS processes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use lutin_agent_sdk::{
    Agent, AgentConfig, FinishReason, LoopConfig, SamplingParams, ToolPolicy, Toolbox,
};
use lutin_llm::mock::{MockProvider, MockResponse};
use lutin_tools::context::ToolContext;
use lutin_tools::read_state::ReadState;
use lutin_tools::shell::Shell;
use tempfile::TempDir;
use tokio::time::sleep;

fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn cancel_kills_long_running_shell_child() {
    let tmp = TempDir::new().unwrap();
    let pid_path = tmp.path().join("pid.txt");

    let shell_ctx = Arc::new(ToolContext {
        root: tmp.path().to_path_buf(),
        env: Arc::from([]),
        http: reqwest::Client::new(),
        read_state: Arc::new(ReadState::new(tmp.path().to_path_buf())),
    });
    let toolbox = Toolbox::new(vec![Box::new(Shell::new(Arc::clone(&shell_ctx)))])
        .expect("single-tool toolbox builds");

    let command = format!("echo $$ > {} && sleep 30", pid_path.display());
    let provider = Arc::new(MockProvider::new(vec![MockResponse::tool_call(
        "c1",
        "shell",
        serde_json::json!({ "command": command, "timeout": 60 }),
    )]));

    let mut agent = Agent::new(AgentConfig {
        provider,
        model: lutin_llm::ModelId::from("mock"),
        sampling: SamplingParams::default(),
        system: "test".into(),
        tool_policy: ToolPolicy::default(),
        loop_config: LoopConfig::default(),
    });
    agent.try_set_tools(toolbox).expect("idle agent accepts tools");
    agent
        .push_message(lutin_llm::Message::User("run something".into()))
        .expect("idle agent accepts messages");

    let mut stream = agent.start().expect("agent is idle");

    // Drain events on a background task: the run task pushes events into
    // an unbounded channel, so this isn't strictly necessary for forward
    // progress, but it mirrors a real consumer and ensures we don't leak
    // unread events on cancel.
    let drain = tokio::spawn(async move {
        while stream.next().await.is_some() {}
    });

    // Wait for bash to write its PID. This is the proof that the agent
    // actually dispatched the shell call and a child process exists.
    let deadline = Instant::now() + Duration::from_secs(10);
    let pid: u32 = loop {
        if Instant::now() > deadline {
            panic!("shell tool never wrote pid.txt — agent did not dispatch the call");
        }
        if let Ok(s) = tokio::fs::read_to_string(&pid_path).await {
            if let Ok(p) = s.trim().parse::<u32>() {
                break p;
            }
        }
        sleep(Duration::from_millis(20)).await;
    };
    assert!(pid_alive(pid), "bash child {pid} should be alive immediately after dispatch");

    // The user-Stop equivalent.
    agent.cancel();

    let outcome = agent.join().await;
    drain.await.expect("drain task did not panic");

    assert!(
        matches!(outcome.finish_reason, FinishReason::Cancelled),
        "expected FinishReason::Cancelled, got {:?}",
        outcome.finish_reason
    );

    let dead_by = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) {
        if Instant::now() > dead_by {
            panic!(
                "bash child {pid} survived agent.cancel() — cancellation did not reach the OS process"
            );
        }
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn cancel_during_llm_stream_finishes_cancelled() {
    // Cancel arriving during the LLM stream phase (no tool call yet) still
    // produces a clean `Cancelled` outcome — covers the early-cancel path
    // separate from the in-flight-tool path above.
    let provider = Arc::new(MockProvider::new(vec![MockResponse::tool_call(
        "c1",
        "shell",
        serde_json::json!({ "command": "sleep 30", "timeout": 60 }),
    )]));
    let mut agent = Agent::new(AgentConfig {
        provider,
        model: lutin_llm::ModelId::from("mock"),
        sampling: SamplingParams::default(),
        system: "test".into(),
        tool_policy: ToolPolicy::default(),
        loop_config: LoopConfig::default(),
    });
    agent
        .push_message(lutin_llm::Message::User("hi".into()))
        .expect("idle accepts");
    let mut stream = agent.start().expect("idle");

    // Cancel before consuming any events — racy, but the worst case is
    // a clean Stopped/Cancelled either way. The test fails only on Error.
    agent.cancel();
    while stream.next().await.is_some() {}
    let outcome = agent.join().await;

    assert!(
        matches!(
            outcome.finish_reason,
            FinishReason::Cancelled | FinishReason::Stopped
        ),
        "expected Cancelled or Stopped, got {:?}",
        outcome.finish_reason
    );
}
