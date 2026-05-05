use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_RESULTS: usize = 500;

pub struct FileGlob {
    ctx: Arc<ToolContext>,
}

impl FileGlob {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("glob"),
        description: "Find files by glob pattern (*, ?, **, [abc], {a,b}). Respects .gitignore. Returns paths relative to the search root.".into(),
        parameters: vec![
            ToolParameter { name: "pattern".into(), r#type: "string".into(), description: "Glob pattern like \"*.rs\", \"src/**/*.toml\", \"test_*\".".into(), required: true },
            ToolParameter { name: "path".into(), r#type: "string".into(), description: "Directory to search in. Defaults to \".\".".into(), required: false },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    pattern: String,
    #[serde(default = "default_path")]
    path: String,
}

fn default_path() -> String {
    ".".into()
}

#[async_trait]
impl crate::Tool for FileGlob {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileGlob {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let root = self.ctx.resolve(&input.path);

        if !root.is_dir() {
            return ToolOutput::err(format!("'{}' is not a directory", input.path));
        }

        let matcher = match Glob::new(&input.pattern) {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::err(format!("invalid glob pattern: {e}")),
        };

        let pattern = input.pattern.clone();
        let root_clone = root.clone();

        let walk_result =
            tokio::task::spawn_blocking(move || collect_matches(&root_clone, matcher)).await;

        let mut matches = match walk_result {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => return ToolOutput::err(format!("failed to search: {e}")),
            Err(e) => return ToolOutput::err(format!("walk task failed: {e}")),
        };

        matches.sort();

        if matches.is_empty() {
            return ToolOutput::ok(format!("no files matching '{pattern}'"));
        }

        let total = matches.len();
        let truncated = total > MAX_RESULTS;
        if truncated {
            matches.truncate(MAX_RESULTS);
        }

        let mut output = matches.join("\n");
        if truncated {
            output.push_str(&format!(
                "\n... (truncated, showing {MAX_RESULTS} of {total} matches)"
            ));
        }

        ToolOutput::ok(output)
    }
}

fn collect_matches(
    root: &PathBuf,
    matcher: globset::GlobMatcher,
) -> Result<Vec<String>, ignore::Error> {
    let mut results = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .parents(false)
        .follow_links(false)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // Per-entry walk errors (permission denied, vanished file,
                // ignore-file parse error) shouldn't abort the whole search;
                // log and skip so the user still gets the rest of the matches.
                tracing::warn!(error = %e, "file_glob: skipping unreadable walk entry");
                continue;
            }
        };

        if entry.depth() == 0 {
            continue;
        }

        let rel = match entry.path().strip_prefix(root) {
            Ok(p) => p,
            Err(e) => {
                // strip_prefix should always succeed for entries returned
                // under `root`; if it doesn't, the walker handed us a path
                // outside the search root, which we can't sensibly report.
                tracing::warn!(
                    path = %entry.path().display(),
                    root = %root.display(),
                    error = %e,
                    "file_glob: entry path not under search root; skipping"
                );
                continue;
            }
        };

        let rel_str: String = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");

        if rel_str.is_empty() {
            continue;
        }

        if matcher.is_match(&rel_str) {
            results.push(rel_str);
        }
    }

    Ok(results)
}
