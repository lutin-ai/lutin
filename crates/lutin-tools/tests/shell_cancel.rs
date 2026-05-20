//! Shell-cancellation regression tests at the tool layer.
//!
//! Verifies that dropping the `Shell::call()` future mid-execution
//! actually kills the spawned bash child. The agent SDK's `agent.cancel()`
//! relies on this drop chain: `tokio::select!` in the run loop drops the
//! in-flight tool future on cancel, which must cascade into killing the
//! OS process. If `kill_on_drop(true)` ever regressed, this test fires.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lutin_llm::{CallId, ToolCall, ToolName};
use lutin_tools::context::ToolContext;
use lutin_tools::read_state::ReadState;
use lutin_tools::shell::Shell;
use lutin_tools::{Tool, ToolCallContext};
use tempfile::TempDir;
use tokio::time::sleep;

fn pid_alive(pid: u32) -> bool {
    // `kill -0` returns success iff the process exists (zombies count as
    // existing). `kill_on_drop` SIGKILLs *and* reaps via `wait`, so a
    // truly cleaned-up process disappears here.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ctx(root: &Path) -> Arc<ToolContext> {
    Arc::new(ToolContext {
        root: root.to_path_buf(),
        env: Arc::from([]),
        http: reqwest::Client::new(),
        read_state: Arc::new(ReadState::new(root.to_path_buf())),
    })
}

async fn wait_until_dead(pid: u32, max: Duration) -> bool {
    let start = Instant::now();
    while pid_alive(pid) {
        if start.elapsed() > max {
            return false;
        }
        sleep(Duration::from_millis(20)).await;
    }
    true
}

async fn read_pid(path: &Path) -> Option<u32> {
    let s = tokio::fs::read_to_string(path).await.ok()?;
    s.trim().parse().ok()
}

#[tokio::test]
async fn dropping_call_future_kills_bash_child() {
    let tmp = TempDir::new().unwrap();
    let shell = Shell::new(ctx(tmp.path()));
    let pid_path = tmp.path().join("pid.txt");

    let call = ToolCall {
        id: CallId::new("c1"),
        name: ToolName::new("shell"),
        arguments: serde_json::json!({
            "command": format!("echo $$ > {} && sleep 30", pid_path.display()),
            "timeout": 60,
        }),
    };

    let call_ctx = ToolCallContext::default();
    let pid: u32 = {
        let mut fut = Box::pin(shell.call(&call_ctx, call));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            tokio::select! {
                _ = &mut fut => panic!("shell finished before pidfile appeared"),
                _ = sleep(Duration::from_millis(20)) => {}
            }
            if let Some(p) = read_pid(&pid_path).await {
                break p;
            }
            if Instant::now() > deadline {
                panic!("bash child never wrote pidfile");
            }
        }
        // `fut` is dropped here — this is the only thing that should kill the child.
    };

    assert!(
        wait_until_dead(pid, Duration::from_secs(5)).await,
        "bash child {pid} survived future drop — kill_on_drop did not fire"
    );
}

#[tokio::test]
async fn timeout_kills_bash_child() {
    // The internal `tokio::time::timeout` path (not user cancel, but the
    // same drop-chain code path) must also kill the bash child.
    let tmp = TempDir::new().unwrap();
    let shell = Shell::new(ctx(tmp.path()));
    let pid_path = tmp.path().join("pid.txt");

    let call = ToolCall {
        id: CallId::new("c1"),
        name: ToolName::new("shell"),
        arguments: serde_json::json!({
            "command": format!("echo $$ > {} && sleep 30", pid_path.display()),
            "timeout": 1,
        }),
    };

    let call_ctx = ToolCallContext::default();
    let started = Instant::now();
    let _outcome = shell.call(&call_ctx, call).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "shell::call returned only after {:?} — timeout failed to break sleep",
        started.elapsed()
    );

    let pid = read_pid(&pid_path)
        .await
        .expect("bash should have written its pid before timing out");
    assert!(
        wait_until_dead(pid, Duration::from_secs(5)).await,
        "bash child {pid} survived shell timeout"
    );
}

/// Currently-failing test that documents the descendant-process gap.
///
/// `kill_on_drop` SIGKILLs the direct bash child, but the `sleep` that
/// bash spawned is in the same process group only by default-inheritance,
/// not by an explicit `setpgid`. When bash dies, the `sleep` is reparented
/// to PID 1 and keeps running until it finishes naturally.
///
/// If `Shell` is ever upgraded to use a fresh process group + group-kill,
/// remove `#[ignore]` and this becomes a regression guard for that fix.
#[tokio::test]
#[ignore = "documents descendant-process gap; un-ignore once shell uses pgid"]
async fn dropping_call_future_kills_grandchild_too() {
    let tmp = TempDir::new().unwrap();
    let shell = Shell::new(ctx(tmp.path()));
    let bash_pid_path = tmp.path().join("bash.pid");
    let sleep_pid_path = tmp.path().join("sleep.pid");

    // `exec sleep` would replace bash, hiding the distinction. We want a
    // real grandchild, so background `sleep` and record its PID.
    let cmd = format!(
        "echo $$ > {bash} && (sleep 30 & echo $! > {slp}) && wait",
        bash = bash_pid_path.display(),
        slp = sleep_pid_path.display(),
    );
    let call = ToolCall {
        id: CallId::new("c1"),
        name: ToolName::new("shell"),
        arguments: serde_json::json!({ "command": cmd, "timeout": 60 }),
    };

    let call_ctx = ToolCallContext::default();
    let (bash_pid, sleep_pid) = {
        let mut fut = Box::pin(shell.call(&call_ctx, call));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            tokio::select! {
                _ = &mut fut => panic!("shell exited before pidfiles appeared"),
                _ = sleep(Duration::from_millis(20)) => {}
            }
            if let (Some(b), Some(s)) =
                (read_pid(&bash_pid_path).await, read_pid(&sleep_pid_path).await)
            {
                break (b, s);
            }
            if Instant::now() > deadline {
                panic!("bash/sleep never wrote pidfiles");
            }
        }
    };

    assert!(
        wait_until_dead(bash_pid, Duration::from_secs(5)).await,
        "bash {bash_pid} survived drop"
    );
    assert!(
        wait_until_dead(sleep_pid, Duration::from_secs(5)).await,
        "sleep grandchild {sleep_pid} survived drop — descendants are leaking past cancel"
    );
}
