use std::sync::Arc;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_WRITE_BYTES: usize = 1024 * 1024;

pub struct FileWrite {
    ctx: Arc<ToolContext>,
}

impl FileWrite {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("write"),
        description: "Write content to a file (max 1 MB). Creates the file if it doesn't exist, overwrites if it does. Overwriting requires a prior read in the same session.".into(),
        parameters: vec![
            ToolParameter { name: "path".into(), r#type: "string".into(), description: "Absolute or relative path to the file.".into(), required: true },
            ToolParameter { name: "content".into(), r#type: "string".into(), description: "The content to write.".into(), required: true },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    path: String,
    content: String,
}

#[async_trait]
impl crate::Tool for FileWrite {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileWrite {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let path = self.ctx.resolve(&input.path);

        if input.content.len() > MAX_WRITE_BYTES {
            return ToolOutput::err(format!(
                "content too large: {} bytes (max {})",
                input.content.len(),
                MAX_WRITE_BYTES
            ));
        }

        let existing = tokio::fs::metadata(&path).await.ok();

        if let Some(meta) = existing.as_ref() {
            match self.ctx.read_state.recorded_mtime(&path) {
                None => {
                    return ToolOutput::err(format!(
                        "{} exists but has not been read this session — call read first to confirm you intend to overwrite its contents",
                        input.path
                    ));
                }
                Some(recorded) => {
                    let current = match meta.modified() {
                        Ok(m) => m,
                        Err(e) => {
                            return ToolOutput::err(format!("failed to stat {}: {e}", input.path));
                        }
                    };
                    if current != recorded {
                        return ToolOutput::err(format!(
                            "{} has changed on disk since it was read (external modification). Re-read before overwriting.",
                            input.path
                        ));
                    }
                }
            }
        }

        let old_size = existing.as_ref().map(|m| m.len());
        let old_line_count = if existing.is_some() {
            match tokio::fs::read(&path).await {
                Ok(bytes) => Some(bytes.iter().filter(|&&b| b == b'\n').count()),
                Err(_) => None,
            }
        } else {
            None
        };

        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::err(format!("failed to create directories: {e}"));
            }
        }

        if let Err(e) = tokio::fs::write(&path, &input.content).await {
            return ToolOutput::err(format!("failed to write {}: {e}", input.path));
        }

        let new_meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => {
                return ToolOutput::err(format!("failed to stat {} after write: {e}", input.path));
            }
        };
        if let Ok(new_mtime) = new_meta.modified() {
            self.ctx.read_state.mark_read(path.clone(), new_mtime);
        }

        let new_line_count = input
            .content
            .as_bytes()
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        let bytes_written = input.content.len();

        let msg = match (old_size, old_line_count) {
            (Some(old_bytes), Some(old_lines)) => format!(
                "wrote {} bytes to {} (overwrote {} bytes, was {} lines, now {} lines)",
                bytes_written, input.path, old_bytes, old_lines, new_line_count
            ),
            _ => format!(
                "wrote {} bytes to {} (new file, {} lines)",
                bytes_written, input.path, new_line_count
            ),
        };

        ToolOutput::ok(msg)
    }
}
