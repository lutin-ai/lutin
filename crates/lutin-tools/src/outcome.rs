//! Internal result helper used by every tool.
//!
//! `ToolOutput` is shaped like `Result<(text, images), text>`: a success
//! variant carries the textual content alongside any optional images, and a
//! failure variant carries only an error string. Encoding the success/failure
//! distinction in the variant (rather than as a separate `is_error: bool`
//! field) makes "error with images" or "success without text" unrepresentable.
//! It converts into `ToolResult::Ok(ToolResultContent)` with the `is_error`
//! bit set from the variant.
//!
//! TODO: images are suffixed to the content as a text note for now. Once
//! `ToolResult` grows an image slot, drop the suffix and pass them through.

use lutin_llm::{CallId, ToolResultContent};

use crate::ToolResult;

pub enum ToolOutput {
    Ok { content: String, images: Vec<String> },
    Err(String),
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self::Ok { content: content.into(), images: Vec::new() }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self::Err(content.into())
    }

    /// Attach images to a success output. Calling this on an `Err` variant
    /// is a no-op — the type forbids errors-with-images.
    pub fn with_images(self, images: Vec<String>) -> Self {
        match self {
            Self::Ok { content, .. } => Self::Ok { content, images },
            err @ Self::Err(_) => err,
        }
    }

    pub fn into_outcome(self, call_id: CallId) -> ToolResult {
        let (mut content, is_error, images) = match self {
            Self::Ok { content, images } => (content, false, images),
            Self::Err(content) => (content, true, Vec::new()),
        };
        if !images.is_empty() {
            content.push_str("\n[images: ");
            content.push_str(&images.join(", "));
            content.push(']');
        }
        ToolResult::Ok(ToolResultContent {
            call_id,
            content,
            is_error,
        })
    }
}
