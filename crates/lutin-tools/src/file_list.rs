use std::sync::Arc;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_ENTRIES: usize = 1000;

pub struct FileList {
    ctx: Arc<ToolContext>,
}

impl FileList {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("list"),
        description: "List entries in a directory (type, name, size).".into(),
        parameters: vec![
            ToolParameter {
                name: "path".into(),
                r#type: "string".into(),
                description: "Directory path to list. Defaults to \".\" (sandbox root).".into(),
                required: false,
            },
            ToolParameter {
                name: "all".into(),
                r#type: "boolean".into(),
                description: "Include hidden files (starting with .). Defaults to false.".into(),
                required: false,
            },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    all: bool,
}

fn default_path() -> String {
    ".".into()
}

struct Entry {
    name: String,
    kind: EntryKind,
    size: u64,
}

enum EntryKind {
    Dir,
    File,
    Symlink { target: String },
}

fn sort_key(kind: &EntryKind) -> u8 {
    match kind {
        EntryKind::Dir => 0,
        EntryKind::Symlink { .. } => 1,
        EntryKind::File => 2,
    }
}

#[async_trait]
impl crate::Tool for FileList {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileList {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let path = self.ctx.resolve(&input.path);

        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(m) => m,
            Err(_) => return ToolOutput::err(format!("directory not found: {}", input.path)),
        };

        if !metadata.is_dir() {
            return ToolOutput::err(format!("not a directory: {}", input.path));
        }

        let mut read_dir = match tokio::fs::read_dir(&path).await {
            Ok(rd) => rd,
            Err(e) => return ToolOutput::err(format!("failed to read directory: {e}")),
        };

        let mut entries = Vec::new();

        while let Some(entry) = read_dir.next_entry().await.transpose() {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => return ToolOutput::err(format!("failed to read entry: {e}")),
            };

            let name = entry.file_name().to_string_lossy().into_owned();

            if !input.all && name.starts_with('.') {
                continue;
            }

            let ft = match entry.file_type().await {
                Ok(ft) => ft,
                Err(e) => return ToolOutput::err(format!("failed to stat entry: {e}")),
            };

            let is_dir = ft.is_dir();
            let is_symlink = ft.is_symlink();

            let (kind, size) = if is_symlink {
                let target = match tokio::fs::read_link(entry.path()).await {
                    Ok(p) => p.to_string_lossy().into_owned(),
                    Err(e) => {
                        // read_link on a symlink itself rarely fails (the
                        // link exists; we just statted it). Surface the
                        // reason in logs and render a tagged target so the
                        // listing stays readable instead of silently lying.
                        tracing::warn!(
                            path = %entry.path().display(),
                            error = %e,
                            "file_list: failed to read symlink target"
                        );
                        format!("[unreadable: {e}]")
                    }
                };
                (EntryKind::Symlink { target }, 0)
            } else if is_dir {
                (EntryKind::Dir, 0)
            } else {
                let size = match entry.metadata().await {
                    Ok(m) => m.len(),
                    Err(_) => 0,
                };
                (EntryKind::File, size)
            };

            entries.push(Entry { name, kind, size });
        }

        if entries.is_empty() {
            return ToolOutput::ok("directory is empty");
        }

        entries.sort_by(|a, b| {
            sort_key(&a.kind)
                .cmp(&sort_key(&b.kind))
                .then_with(|| a.name.cmp(&b.name))
        });

        let mut dir_count = 0usize;
        let mut file_count = 0usize;
        let mut link_count = 0usize;
        let mut total_bytes = 0u64;
        for entry in &entries {
            match &entry.kind {
                EntryKind::Dir => dir_count += 1,
                EntryKind::File => {
                    file_count += 1;
                    total_bytes += entry.size;
                }
                EntryKind::Symlink { .. } => link_count += 1,
            }
        }

        let total = entries.len();
        let truncated = total > MAX_ENTRIES;
        if truncated {
            entries.truncate(MAX_ENTRIES);
        }

        let mut listing = entries
            .iter()
            .map(format_entry)
            .collect::<Vec<_>>()
            .join("\n");

        if truncated {
            listing.push_str(&format!(
                "\n... (truncated, showing {MAX_ENTRIES} of {total} entries)"
            ));
        }

        let output = format!(
            "{listing}\n{dir_count} dirs, {file_count} files, {link_count} links, total {total_bytes} bytes"
        );

        ToolOutput::ok(output)
    }
}

fn format_entry(entry: &Entry) -> String {
    match &entry.kind {
        EntryKind::Dir => format!("{}/", entry.name),
        EntryKind::File => format!("{} ({} bytes)", entry.name, entry.size),
        EntryKind::Symlink { target } => format!("{} → {}", entry.name, target),
    }
}
