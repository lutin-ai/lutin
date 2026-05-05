use std::fmt::Write as _;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_READ_LINES: usize = 2000;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 2000;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

pub struct FileRead {
    ctx: Arc<ToolContext>,
}

impl FileRead {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("read"),
        description: "Read lines from a file with cat -n style line numbers. Returns up to 2000 lines per call. Use `start` (1-based) to page through larger files.".into(),
        parameters: vec![
            ToolParameter { name: "path".into(), r#type: "string".into(), description: "Absolute or relative path to the file.".into(), required: true },
            ToolParameter { name: "start".into(), r#type: "integer".into(), description: "1-based line number to start reading from. Defaults to 1.".into(), required: false },
            ToolParameter { name: "limit".into(), r#type: "integer".into(), description: "Maximum lines to read (max 2000). Defaults to 2000.".into(), required: false },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    path: String,
    start: Option<usize>,
    limit: Option<usize>,
}

fn emit_line(out: &mut String, lineno: usize, content: &str) {
    let char_count = content.chars().count();
    let _ = write!(out, "{:>4}\t", lineno);
    if char_count > MAX_LINE_CHARS {
        let truncated: String = content.chars().take(MAX_LINE_CHARS).collect();
        let trimmed = char_count - MAX_LINE_CHARS;
        let _ = write!(out, "{truncated}…[+{trimmed} chars]");
    } else {
        out.push_str(content);
    }
}

#[async_trait]
impl crate::Tool for FileRead {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileRead {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let path = self.ctx.resolve(&input.path);

        let limit = input.limit.unwrap_or(MAX_READ_LINES);
        if limit == 0 {
            return ToolOutput::err("limit must be at least 1".to_string());
        }
        let limit = limit.min(MAX_READ_LINES);
        let start = input.start.unwrap_or(1).max(1);

        let metadata = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => return ToolOutput::err(format!("failed to open {}: {e}", input.path)),
        };

        if metadata.len() > MAX_FILE_BYTES {
            return ToolOutput::err(format!(
                "file {} too large ({} bytes, max {})",
                input.path,
                metadata.len(),
                MAX_FILE_BYTES
            ));
        }

        let mtime = metadata.modified().unwrap_or_else(|_| SystemTime::now());

        if metadata.len() == 0 {
            self.ctx.read_state.mark_read(path.clone(), mtime);
            return ToolOutput::ok("[empty file]");
        }

        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(format!("failed to read {}: {e}", input.path)),
        };

        let sniff_len = bytes.len().min(BINARY_SNIFF_BYTES);
        if bytes[..sniff_len].contains(&0u8) {
            return ToolOutput::err(format!(
                "{} appears to be binary (null bytes detected). Use file_view tools or a shell hexdump if needed.",
                input.path
            ));
        }

        let content = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(e) => {
                return ToolOutput::err(format!(
                    "failed to read {}: invalid UTF-8 at byte {}",
                    input.path,
                    e.valid_up_to()
                ));
            }
        };

        let total = content.lines().count();

        if start > total {
            return ToolOutput::err(format!("start={start} exceeds file length ({total} lines)"));
        }

        let mut body = String::new();
        let mut emitted: Option<(usize, usize)> = None;
        for (idx, line) in content.lines().enumerate().skip(start - 1).take(limit) {
            let lineno = idx + 1;
            if emitted.is_some() {
                body.push('\n');
            }
            emit_line(&mut body, lineno, line);
            emitted = Some(match emitted {
                Some((first, _)) => (first, lineno),
                None => (lineno, lineno),
            });
        }

        self.ctx.read_state.mark_read(path.clone(), mtime);

        let (first_emitted, last_emitted) =
            emitted.expect("non-empty file with start<=total must emit >=1 line");
        let has_more = last_emitted < total;
        let output = if has_more {
            let remaining = total - last_emitted;
            format!("{body}\n[{first_emitted}-{last_emitted} of {total} lines, {remaining} more]")
        } else {
            body
        };

        ToolOutput::ok(output)
    }
}
