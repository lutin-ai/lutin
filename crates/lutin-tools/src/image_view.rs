use std::sync::Arc;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

pub struct ImageView {
    ctx: Arc<ToolContext>,
}

impl ImageView {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("image_view"),
        description: "Load an image file and attach it to the conversation so the model can see it. Accepts PNG, JPEG, GIF, and WebP. The path is resolved relative to the sandbox.".into(),
        parameters: vec![ToolParameter {
            name: "path".into(),
            r#type: "string".into(),
            description: "Sandbox-relative path to the image file.".into(),
            required: true,
        }],
    }
}

#[derive(Deserialize)]
struct Input {
    path: String,
}

#[async_trait]
impl crate::Tool for ImageView {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl ImageView {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let abs = self.ctx.resolve(&input.path);

        let metadata = match tokio::fs::metadata(&abs).await {
            Ok(m) => m,
            Err(e) => return ToolOutput::err(format!("failed to stat {}: {e}", input.path)),
        };

        if metadata.len() > MAX_IMAGE_BYTES {
            return ToolOutput::err(format!(
                "image too large ({} bytes, max {})",
                metadata.len(),
                MAX_IMAGE_BYTES
            ));
        }

        let ext = std::path::Path::new(&input.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
            return ToolOutput::err(format!(
                "unsupported image extension: {ext:?} (use png, jpg, gif, or webp)"
            ));
        }

        let filename = std::path::Path::new(&input.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&input.path);

        ToolOutput::ok(format!(
            "Loaded image {} ({} bytes). Attached below.",
            filename,
            metadata.len()
        ))
        .with_images(vec![input.path])
    }
}
