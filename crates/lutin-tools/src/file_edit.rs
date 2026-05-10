use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

pub struct FileEdit {
    ctx: Arc<ToolContext>,
}

impl FileEdit {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("edit"),
        description: "Find-and-replace in a file. Returns content where each line is prefixed with a right-aligned line number and a tab (e.g. `  42\\t<content>`), and the output ends with a `[X-Y of N lines, M more]` footer. old_string and new_string must contain ONLY the raw file content — never include the line-number prefix, the tab, or the footer. Preserve the file's exact indentation (tabs/spaces) as it appears AFTER the line-number + tab prefix.".into(),
        parameters: vec![
            ToolParameter { name: "path".into(), r#type: "string".into(), description: "Absolute or relative path to the file.".into(), required: true },
            ToolParameter { name: "old_string".into(), r#type: "string".into(), description: "Exact text to find in the file. Must be unique unless replace_all is true.".into(), required: true },
            ToolParameter { name: "new_string".into(), r#type: "string".into(), description: "Text to replace old_string with. Use empty string to delete.".into(), required: true },
            ToolParameter { name: "replace_all".into(), r#type: "boolean".into(), description: "If true, replace every occurrence of old_string. Default false.".into(), required: false },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

enum FindResult {
    Unique(usize),
    Multiple(usize),
    NotFound,
}

fn find_match(haystack: &str, needle: &str) -> FindResult {
    let mut iter = haystack.match_indices(needle);
    let Some((first, _)) = iter.next() else {
        return FindResult::NotFound;
    };
    let extra = iter.count();
    if extra == 0 {
        FindResult::Unique(first)
    } else {
        FindResult::Multiple(extra + 1)
    }
}

fn crlf_mismatch(contents: &str, needle: &str) -> bool {
    let file_has_crlf = contents.contains("\r\n");
    let file_has_lf = contents
        .as_bytes()
        .windows(1)
        .enumerate()
        .any(|(i, b)| b[0] == b'\n' && (i == 0 || contents.as_bytes()[i - 1] != b'\r'));
    let needle_has_crlf = needle.contains("\r\n");
    let needle_has_lf = needle
        .as_bytes()
        .windows(1)
        .enumerate()
        .any(|(i, b)| b[0] == b'\n' && (i == 0 || needle.as_bytes()[i - 1] != b'\r'));
    (file_has_crlf && needle_has_lf && !needle_has_crlf)
        || (needle_has_crlf && file_has_lf && !file_has_crlf)
}

fn normalize_crlf(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len() + 1);
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if c == '\r' {
            if let Some(&(_, '\n')) = iter.peek() {
                iter.next();
                map.push(i);
                out.push('\n');
                continue;
            }
        }
        let clen = c.len_utf8();
        for k in 0..clen {
            map.push(i + k);
        }
        out.push(c);
    }
    map.push(s.len());
    (out, map)
}

fn denormalize_to_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.matches('\n').count());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c == '\n' && prev != Some('\r') {
            out.push('\r');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

fn line_number_at(contents: &str, offset: usize) -> usize {
    contents[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

fn preview_line(new_string: &str) -> String {
    if new_string.is_empty() {
        return "(deleted)".to_string();
    }
    let first = new_string.lines().next().unwrap_or("");
    const MAX: usize = 80;
    if first.chars().count() <= MAX {
        first.to_string()
    } else {
        let truncated: String = first.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}

fn line_ending_label(contents: &str) -> &'static str {
    if contents.contains("\r\n") {
        "CRLF"
    } else {
        "LF"
    }
}

#[async_trait]
impl crate::Tool for FileEdit {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileEdit {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        if input.old_string.is_empty() {
            return ToolOutput::err("old_string must not be empty");
        }

        // Some models (notably Qwen3-Coder under vLLM) emit edits where
        // old_string and new_string are byte-identical — a silent no-op
        // that previously reported "edited N occurrences" while the file
        // stayed unchanged. Surface it as an error so the model gets a
        // chance to self-correct instead of marching on under the
        // impression the change landed.
        if input.old_string == input.new_string {
            return ToolOutput::err(
                "old_string and new_string are identical — this would be a no-op. \
                 If you meant to delete, pass an empty new_string. \
                 Otherwise re-read the file and provide the exact differing bytes.",
            );
        }

        let path = self.ctx.resolve(&input.path);

        let metadata = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::err(format!(
                    "{} does not exist — use file_write to create new files",
                    input.path
                ));
            }
            Err(e) => return ToolOutput::err(format!("failed to stat {}: {e}", input.path)),
        };

        let current_mtime = metadata.modified().unwrap_or_else(|_| SystemTime::now());

        let Some(recorded) = self.ctx.read_state.recorded_mtime(&path) else {
            return ToolOutput::err(format!(
                "{} has not been read this session — call read first so edits can be verified against the known contents",
                input.path
            ));
        };

        if recorded != current_mtime {
            return ToolOutput::err(format!(
                "{} has changed on disk since it was read (external modification). Re-read before editing.",
                input.path
            ));
        }

        let contents = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("failed to read {}: {e}", input.path)),
        };

        let primary = find_match(&contents, &input.old_string);

        if input.replace_all {
            let (new_contents, count) = match primary {
                FindResult::Unique(_) => {
                    let replaced = contents.replace(&input.old_string, &input.new_string);
                    (replaced, 1)
                }
                FindResult::Multiple(count) => {
                    let replaced = contents.replace(&input.old_string, &input.new_string);
                    (replaced, count)
                }
                FindResult::NotFound => {
                    if !crlf_mismatch(&contents, &input.old_string) {
                        return ToolOutput::err("old_string not found in file");
                    }
                    let (norm_contents, _) = normalize_crlf(&contents);
                    let norm_needle = input.old_string.replace("\r\n", "\n");
                    let norm_new = input.new_string.replace("\r\n", "\n");
                    let n = norm_contents.matches(&norm_needle).count();
                    if n == 0 {
                        return ToolOutput::err(format!(
                            "old_string not found in file\n(hint: file uses {} line endings — ensure old_string matches exactly including line endings)",
                            line_ending_label(&contents)
                        ));
                    }
                    let replaced_norm = norm_contents.replace(&norm_needle, &norm_new);
                    let replaced = if contents.contains("\r\n") {
                        denormalize_to_crlf(&replaced_norm)
                    } else {
                        replaced_norm
                    };
                    (replaced, n)
                }
            };

            return write_and_report(
                &self.ctx,
                &path,
                &input.path,
                &new_contents,
                format!(
                    "edited {}: replaced {count} occurrence{}",
                    input.path,
                    if count == 1 { "" } else { "s" }
                ),
            )
            .await;
        }

        let match_offset = match primary {
            FindResult::Unique(offset) => offset,
            FindResult::Multiple(count) => {
                return ToolOutput::err(format!(
                    "old_string is not unique in file (found {count} occurrences). Provide more context to make it unique, or pass replace_all: true."
                ));
            }
            FindResult::NotFound => {
                if crlf_mismatch(&contents, &input.old_string) {
                    let (norm_contents, map) = normalize_crlf(&contents);
                    let norm_needle = input.old_string.replace("\r\n", "\n");
                    match find_match(&norm_contents, &norm_needle) {
                        FindResult::Unique(norm_off) => map[norm_off],
                        FindResult::Multiple(count) => {
                            return ToolOutput::err(format!(
                                "old_string is not unique in file (found {count} occurrences). Provide more context to make it unique, or pass replace_all: true."
                            ));
                        }
                        FindResult::NotFound => {
                            return ToolOutput::err(format!(
                                "old_string not found in file\n(hint: file uses {} line endings — ensure old_string matches exactly including line endings)",
                                line_ending_label(&contents)
                            ));
                        }
                    }
                } else {
                    return ToolOutput::err("old_string not found in file");
                }
            }
        };

        let matched_len = matched_length_at(&contents, match_offset, &input.old_string);
        let mut new_contents =
            String::with_capacity(contents.len() - matched_len + input.new_string.len());
        new_contents.push_str(&contents[..match_offset]);
        new_contents.push_str(&input.new_string);
        new_contents.push_str(&contents[match_offset + matched_len..]);

        let line_no = line_number_at(&contents, match_offset);
        let preview = preview_line(&input.new_string);
        let msg = format!("edited {}:{line_no}  →  {preview}", input.path);

        write_and_report(&self.ctx, &path, &input.path, &new_contents, msg).await
    }
}

fn matched_length_at(contents: &str, offset: usize, needle: &str) -> usize {
    let cb = contents.as_bytes();
    let nb = needle.as_bytes();
    let mut ci = offset;
    let mut ni = 0;
    while ni < nb.len() {
        if ci >= cb.len() {
            return needle.len();
        }
        if cb[ci] == nb[ni] {
            ci += 1;
            ni += 1;
        } else if nb[ni] == b'\n' && cb[ci] == b'\r' && ci + 1 < cb.len() && cb[ci + 1] == b'\n' {
            ci += 2;
            ni += 1;
        } else if cb[ci] == b'\n' && nb[ni] == b'\r' && ni + 1 < nb.len() && nb[ni + 1] == b'\n' {
            ci += 1;
            ni += 2;
        } else {
            return needle.len();
        }
    }
    ci - offset
}

async fn write_and_report(
    ctx: &Arc<ToolContext>,
    path: &Path,
    display_path: &str,
    new_contents: &str,
    msg: String,
) -> ToolOutput {
    if let Err(e) = tokio::fs::write(path, new_contents).await {
        return ToolOutput::err(format!("failed to write {}: {e}", display_path));
    }

    match tokio::fs::metadata(path).await {
        Ok(m) => {
            let mtime = m.modified().unwrap_or_else(|_| SystemTime::now());
            ctx.read_state.mark_read(path.to_path_buf(), mtime);
        }
        Err(_) => {
            ctx.read_state.forget(path);
        }
    }

    ToolOutput::ok(msg)
}
