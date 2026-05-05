use std::sync::Arc;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

/// Hard cap on bytes read from each of stdout/stderr (1 MB).
const READ_CAP_BYTES: u64 = 1024 * 1024;
/// Maximum characters of combined output returned to the model.
const MAX_OUTPUT_CHARS: usize = 30_000;
/// Characters of stdout/stderr shown as a sample when output is too long.
const SAMPLE_CHARS: usize = 1_000;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MIN_TIMEOUT_SECS: u64 = 1;
const MAX_TIMEOUT_SECS: u64 = 600;

pub struct Shell {
    ctx: Arc<ToolContext>,
}

impl Shell {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("shell"),
        description: "Run a bash command in the sandbox.".into(),
        parameters: vec![
            ToolParameter {
                name: "command".into(),
                r#type: "string".into(),
                description: "The shell command to execute.".into(),
                required: true,
            },
            ToolParameter {
                name: "timeout".into(),
                r#type: "integer".into(),
                description: "Timeout in seconds (1-600).".into(),
                required: false,
            },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
}

#[async_trait]
impl crate::Tool for Shell {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl Shell {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let timeout_secs = input
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let timeout = std::time::Duration::from_secs(timeout_secs);

        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(format!("set -o pipefail\n{}", input.command))
            .current_dir(&self.ctx.root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        for (k, v) in self.ctx.env.iter() {
            cmd.env(k, v);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("failed to spawn command: {e}")),
        };

        match tokio::time::timeout(timeout, collect_output(&mut child)).await {
            Ok(Ok((status, stdout, stderr))) => {
                let total_chars = stdout.chars().count() + stderr.chars().count();
                if total_chars > MAX_OUTPUT_CHARS {
                    return ToolOutput::err(format!(
                        "output too long ({total_chars} chars, limit {MAX_OUTPUT_CHARS}). \
                         exit code {code}. refine command (grep/head/tail/redirect to file).\n\
                         --- stdout sample ({sample} chars) ---\n{stdout_sample}\n\
                         --- stderr sample ({sample} chars) ---\n{stderr_sample}",
                        code = status.code().unwrap_or(-1),
                        sample = SAMPLE_CHARS,
                        stdout_sample = take_chars(&stdout, SAMPLE_CHARS),
                        stderr_sample = take_chars(&stderr, SAMPLE_CHARS),
                    ));
                }

                if status.success() {
                    if stdout.is_empty() {
                        ToolOutput::ok("command completed with no output")
                    } else {
                        ToolOutput::ok(stdout)
                    }
                } else {
                    let code = status.code().unwrap_or(-1);
                    let mut msg = format!("exit code {code}");
                    if !stderr.is_empty() {
                        msg.push('\n');
                        msg.push_str(&stderr);
                    }
                    if !stdout.is_empty() {
                        msg.push('\n');
                        msg.push_str(&stdout);
                    }
                    ToolOutput::err(msg)
                }
            }
            Ok(Err(e)) => ToolOutput::err(format!("command execution failed: {e}")),
            Err(_) => {
                // The child has `kill_on_drop(true)`, so a kill failure here
                // isn't catastrophic — the process will still be reaped when
                // `child` is dropped. But a kill failure is unusual (the
                // child has already exited, or we lack permission) and worth
                // surfacing so the user understands why their timeout
                // message arrived without the process actually dying yet.
                let mut msg = format!("command timed out after {timeout_secs}s");
                if let Err(e) = child.kill().await {
                    tracing::warn!(error = %e, "shell: failed to kill timed-out child");
                    msg.push_str(&format!(" (kill failed: {e}; relying on kill_on_drop)"));
                }
                ToolOutput::err(msg)
            }
        }
    }
}

async fn collect_output(
    child: &mut tokio::process::Child,
) -> Result<(std::process::ExitStatus, String, String), std::io::Error> {
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let stdout_fut = async {
        let mut buf = Vec::with_capacity(8192);
        if let Some(mut r) = stdout_handle {
            let _ = (&mut r).take(READ_CAP_BYTES).read_to_end(&mut buf).await;
        }
        buf
    };

    let stderr_fut = async {
        let mut buf = Vec::with_capacity(1024);
        if let Some(mut r) = stderr_handle {
            let _ = (&mut r).take(READ_CAP_BYTES).read_to_end(&mut buf).await;
        }
        buf
    };

    let (stdout_buf, stderr_buf) = tokio::join!(stdout_fut, stderr_fut);
    let status = child.wait().await?;

    Ok((
        status,
        String::from_utf8_lossy(&stdout_buf).into_owned(),
        String::from_utf8_lossy(&stderr_buf).into_owned(),
    ))
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
