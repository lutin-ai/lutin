use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_ENTRIES: usize = 1000;

pub struct FileTree {
    ctx: Arc<ToolContext>,
}

impl FileTree {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("tree"),
        description:
            "Directory tree up to max_depth. Excludes hidden files and respects .gitignore.".into(),
        parameters: vec![
            ToolParameter {
                name: "path".into(),
                r#type: "string".into(),
                description: "Directory path to list. Defaults to the working directory.".into(),
                required: false,
            },
            ToolParameter {
                name: "depth".into(),
                r#type: "integer".into(),
                description: "Maximum recursion depth (1-10). Defaults to 3.".into(),
                required: false,
            },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    #[serde(default = "default_path")]
    path: String,
    #[serde(default = "default_depth")]
    depth: i64,
}

fn default_path() -> String {
    ".".into()
}

fn default_depth() -> i64 {
    3
}

/// Whether a path being matched against gitignore is a directory or a file.
/// Replaces a bare `bool` parameter at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Dir,
}

impl EntryKind {
    fn from_is_dir(is_dir: bool) -> Self {
        if is_dir { EntryKind::Dir } else { EntryKind::File }
    }

    fn is_dir(self) -> bool {
        matches!(self, EntryKind::Dir)
    }
}

struct WalkState {
    lines: Vec<String>,
    total: usize,
    ignores: Vec<Gitignore>,
}

impl WalkState {
    fn new(root: &Path) -> Self {
        Self {
            lines: Vec::new(),
            total: 0,
            ignores: vec![load_gitignore(root)],
        }
    }

    fn is_ignored(&self, path: &Path, kind: EntryKind) -> bool {
        self.is_ignored_with_extra(path, kind, None)
    }

    fn is_ignored_with_extra(
        &self,
        path: &Path,
        kind: EntryKind,
        extra: Option<&Gitignore>,
    ) -> bool {
        let is_dir = kind.is_dir();
        if let Some(g) = extra {
            let m = g.matched(path, is_dir);
            if m.is_ignore() {
                return true;
            }
            if m.is_whitelist() {
                return false;
            }
        }
        for gi in self.ignores.iter().rev() {
            let m = gi.matched(path, is_dir);
            if m.is_ignore() {
                return true;
            }
            if m.is_whitelist() {
                return false;
            }
        }
        false
    }
}

fn load_gitignore(dir: &Path) -> Gitignore {
    let mut b = GitignoreBuilder::new(dir);
    let _ = b.add(dir.join(".gitignore"));
    b.build().unwrap_or_else(|_| Gitignore::empty())
}

fn dir_has_visible_children(dir: &Path, state: &WalkState) -> bool {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    let child_gi = load_gitignore(dir);
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let kind = EntryKind::from_is_dir(matches!(entry.file_type(), Ok(ft) if ft.is_dir()));
        if state.is_ignored_with_extra(&entry.path(), kind, Some(&child_gi)) {
            continue;
        }
        return true;
    }
    false
}

// Work items for the iterative traversal. We process a stack LIFO; to preserve
// the original DFS pre-order (each dir's full subtree before sibling files),
// we push items in reverse order of intended processing.
enum Frame {
    // Read `dir` and emit its dir-children's headers and file-children's lines,
    // queuing subdirectory descent in between.
    Enter {
        dir: PathBuf,
        depth_remaining: usize,
        indent: String,
        // Whether a gitignore for this directory was pushed onto state.ignores
        // when this Enter frame was queued. The root frame uses `false` since
        // its gitignore is set up by WalkState::new.
        pushed_ignore: bool,
    },
    // Emit the header line for a directory and then enqueue its Enter frame
    // (with its gitignore pushed). Used for non-root subdirectories so the
    // parent's file lines can be sequenced after the subtree.
    DirHeader {
        path: PathBuf,
        depth_remaining: usize,
        indent: String,
    },
    // Emit pre-formatted file lines belonging to a directory after that
    // directory's subtree has been fully visited.
    Files {
        lines: Vec<String>,
    },
    // Pop the gitignore that was pushed for a directory whose subtree has now
    // been fully processed.
    PopIgnore,
}

fn walk_dir(root: &Path, depth: usize, root_indent: &str, state: &mut WalkState) {
    let mut stack: Vec<Frame> = Vec::new();
    stack.push(Frame::Enter {
        dir: root.to_path_buf(),
        depth_remaining: depth,
        indent: root_indent.to_string(),
        pushed_ignore: false,
    });

    while let Some(frame) = stack.pop() {
        match frame {
            Frame::PopIgnore => {
                state.ignores.pop();
            }
            Frame::Files { lines } => {
                for line in lines {
                    state.total += 1;
                    if state.lines.len() < MAX_ENTRIES {
                        state.lines.push(line);
                    }
                }
            }
            Frame::DirHeader {
                path,
                depth_remaining,
                indent,
            } => {
                state.total += 1;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if state.lines.len() < MAX_ENTRIES {
                    state.lines.push(format!("{}{}/", indent, name));
                }

                if depth_remaining > 1 {
                    let child_indent = format!("{}  ", indent);
                    state.ignores.push(load_gitignore(&path));
                    stack.push(Frame::Enter {
                        dir: path,
                        depth_remaining: depth_remaining - 1,
                        indent: child_indent,
                        pushed_ignore: true,
                    });
                } else if dir_has_visible_children(&path, state) {
                    state.total += 1;
                    if state.lines.len() < MAX_ENTRIES {
                        state.lines.push(format!("{}  ...", indent));
                    }
                }
            }
            Frame::Enter {
                dir,
                depth_remaining,
                indent,
                pushed_ignore,
            } => {
                let read_dir = match std::fs::read_dir(&dir) {
                    Ok(rd) => Some(rd),
                    Err(_) => None,
                };

                let mut dirs: Vec<PathBuf> = Vec::new();
                let mut files: Vec<PathBuf> = Vec::new();

                if let Some(rd) = read_dir {
                    for entry in rd.flatten() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str.starts_with('.') {
                            continue;
                        }
                        let path = entry.path();
                        let is_dir = matches!(entry.file_type(), Ok(ft) if ft.is_dir());
                        if state.is_ignored(&path, EntryKind::from_is_dir(is_dir)) {
                            continue;
                        }
                        if is_dir {
                            dirs.push(path);
                        } else {
                            files.push(path);
                        }
                    }
                }

                dirs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
                files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

                // Pre-format file lines so we don't need to retain `indent`
                // after the dir subtrees finish.
                let file_lines: Vec<String> = files
                    .iter()
                    .map(|path| {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        format!("{}{}", indent, name)
                    })
                    .collect();

                // Schedule (in reverse so processing order matches recursion):
                //   1. PopIgnore for this Enter (if we pushed one)
                //   2. Files
                //   3. DirHeader for each subdirectory in order
                if pushed_ignore {
                    stack.push(Frame::PopIgnore);
                }
                if !file_lines.is_empty() {
                    stack.push(Frame::Files { lines: file_lines });
                }
                for path in dirs.into_iter().rev() {
                    stack.push(Frame::DirHeader {
                        path,
                        depth_remaining,
                        indent: indent.clone(),
                    });
                }
            }
        }
    }
}

#[async_trait]
impl crate::Tool for FileTree {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileTree {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let path = self.ctx.resolve(&input.path);

        if !path.is_dir() {
            return ToolOutput::err(format!("not a directory: {}", input.path));
        }

        let depth = (input.depth.clamp(1, 10)) as usize;

        let (lines, total) = match tokio::task::spawn_blocking(move || {
            let mut state = WalkState::new(&path);
            walk_dir(&path, depth, "", &mut state);
            (state.lines, state.total)
        })
        .await
        {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("walk join error: {e}")),
        };

        let mut output = lines.join("\n");

        if total > MAX_ENTRIES {
            output.push_str(&format!("\n... (truncated, {} entries)", total));
        }

        ToolOutput::ok(output)
    }
}
