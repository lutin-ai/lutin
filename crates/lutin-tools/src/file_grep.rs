use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use regex::Regex;
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_MATCHES: usize = 200;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

pub struct FileGrep {
    ctx: Arc<ToolContext>,
}

impl FileGrep {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("grep"),
        description: "Regex search over file contents. Respects .gitignore. Skips hidden/binary files. Returns {file}:{line}:{content} matches.".into(),
        parameters: vec![
            ToolParameter { name: "pattern".into(), r#type: "string".into(), description: "Regex pattern to search for.".into(), required: true },
            ToolParameter { name: "path".into(), r#type: "string".into(), description: "File or directory to search in. Defaults to \".\".".into(), required: false },
            ToolParameter { name: "glob".into(), r#type: "string".into(), description: "Filter files by glob pattern (e.g. \"*.rs\").".into(), required: false },
            ToolParameter { name: "context".into(), r#type: "integer".into(), description: "Number of lines of context before and after each match (0-10). Defaults to 0.".into(), required: false },
            ToolParameter { name: "case_insensitive".into(), r#type: "boolean".into(), description: "Case insensitive search. Defaults to false.".into(), required: false },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    pattern: String,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    context: Option<u32>,
    #[serde(default)]
    case_insensitive: bool,
}

fn default_path() -> String {
    ".".into()
}

struct Line {
    rel_path: String,
    line_number: usize,
    line_content: String,
    separator: bool,
}

impl Line {
    fn sep() -> Self {
        Self {
            rel_path: String::new(),
            line_number: 0,
            line_content: String::new(),
            separator: true,
        }
    }
}

struct Scan {
    lines: Vec<Line>,
    total_matches: usize,
}

#[async_trait]
impl crate::Tool for FileGrep {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileGrep {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let re = match regex::RegexBuilder::new(&input.pattern)
            .case_insensitive(input.case_insensitive)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("invalid regex: {e}")),
        };

        let resolved = self.ctx.resolve(&input.path);
        let sandbox_root = self.ctx.root.clone();

        if !resolved.exists() {
            return ToolOutput::err(format!("path not found: {}", input.path));
        }

        let context_lines = input.context.unwrap_or(0).clamp(0, 10) as usize;

        let glob_matcher = match input.glob.as_deref() {
            Some(g) => match Glob::new(g) {
                Ok(glob) => Some(glob.compile_matcher()),
                Err(e) => return ToolOutput::err(format!("invalid glob: {e}")),
            },
            None => None,
        };

        let pattern_display = input.pattern.clone();

        let scan = tokio::task::spawn_blocking(move || {
            walk_and_search(
                &resolved,
                &sandbox_root,
                &re,
                glob_matcher.as_ref(),
                context_lines,
            )
        })
        .await;

        let scan = match scan {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(format!("search task failed: {e}")),
        };

        if scan.total_matches == 0 {
            return ToolOutput::ok(format!("no matches for '{}'", pattern_display));
        }

        let output = format_lines(&scan.lines);

        if scan.total_matches > MAX_MATCHES {
            ToolOutput::ok(format!(
                "{output}\n... (truncated, {MAX_MATCHES} matches shown, more exist)"
            ))
        } else {
            ToolOutput::ok(output)
        }
    }
}

fn walk_and_search(
    target: &Path,
    root: &Path,
    re: &Regex,
    glob: Option<&GlobMatcher>,
    context_lines: usize,
) -> Scan {
    let mut scan = Scan {
        lines: Vec::new(),
        total_matches: 0,
    };

    if target.is_file() {
        search_file(target, root, re, glob, context_lines, &mut scan);
        return scan;
    }

    let walker = WalkBuilder::new(target)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .parents(false)
        .follow_links(false)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        search_file(path, root, re, glob, context_lines, &mut scan);
        if scan.total_matches > MAX_MATCHES {
            break;
        }
    }

    scan
}

fn glob_ok(path: &Path, glob: Option<&GlobMatcher>) -> bool {
    match glob {
        None => true,
        Some(m) => m.is_match(path) || path.file_name().is_some_and(|n| m.is_match(n)),
    }
}

fn is_binary_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; BINARY_SNIFF_BYTES];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    buf[..n].contains(&0)
}

fn relative_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn search_file(
    path: &Path,
    root: &Path,
    re: &Regex,
    glob: Option<&GlobMatcher>,
    context_lines: usize,
    scan: &mut Scan,
) {
    if !glob_ok(path, glob) {
        return;
    }

    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() > MAX_FILE_BYTES {
        return;
    }

    if is_binary_file(path) {
        return;
    }

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let rel = relative_path(path, root);
    let lines: Vec<&str> = content.lines().collect();

    let mut match_indices: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line) {
            scan.total_matches += 1;
            if scan.total_matches <= MAX_MATCHES {
                match_indices.push(i);
            }
        }
    }

    if match_indices.is_empty() {
        return;
    }

    if context_lines == 0 {
        for idx in match_indices {
            scan.lines.push(Line {
                rel_path: rel.clone(),
                line_number: idx + 1,
                line_content: lines[idx].to_string(),
                separator: false,
            });
        }
        return;
    }

    let line_count = lines.len();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for idx in match_indices {
        let start = idx.saturating_sub(context_lines);
        let end = (idx + context_lines).min(line_count.saturating_sub(1));

        if let Some(last) = ranges.last_mut() {
            if start <= last.1 + 1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        ranges.push((start, end));
    }

    let last = ranges.len().saturating_sub(1);
    for (gi, (start, end)) in ranges.iter().enumerate() {
        for i in *start..=*end {
            scan.lines.push(Line {
                rel_path: rel.clone(),
                line_number: i + 1,
                line_content: lines[i].to_string(),
                separator: false,
            });
        }
        if gi < last {
            scan.lines.push(Line::sep());
        }
    }
    scan.lines.push(Line::sep());
}

fn format_lines(lines: &[Line]) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    let mut emitted_any = false;

    for l in lines {
        if l.separator {
            if emitted_any {
                pending_sep = true;
            }
            continue;
        }
        if pending_sep {
            out.push_str("\n--\n");
            pending_sep = false;
        } else if emitted_any {
            out.push('\n');
        }
        out.push_str(&l.rel_path);
        out.push(':');
        out.push_str(&l.line_number.to_string());
        out.push_str(": ");
        out.push_str(&l.line_content);
        emitted_any = true;
    }
    out
}
