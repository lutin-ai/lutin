use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

pub struct FileEditLines {
    ctx: Arc<ToolContext>,
}

impl FileEditLines {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("edit_lines"),
        description: "Edit a file by line numbers. \
                      The full lines are replaced — do not include the line-number prefix or tab in `content`. \
                      To delete lines, pass an empty `content`. \
                      To insert before line N without replacing, pass `N,N-1`. \
                      To append at end of file, pass `total_lines + 1,total_lines`. \
                      A trailing newline is appended to `content` automatically when needed so the \
                      file's line structure is preserved (matches the file's existing CRLF/LF style)."
            .into(),
        parameters: vec![
            ToolParameter {
                name: "path".into(),
                r#type: "string".into(),
                description: "Absolute or relative path to the file.".into(),
                required: true,
            },
            ToolParameter {
                name: "lines".into(),
                r#type: "string".into(),
                description: "1-based inclusive line range as `start,end` (e.g. `12,18`). \
                              `end` may equal `start - 1` for pure insertion before `start`. \
                              `end` past the last line is silently clamped to the last line."
                    .into(),
                required: true,
            },
            ToolParameter {
                name: "content".into(),
                r#type: "string".into(),
                description: "Replacement text for the line range. May span multiple lines. \
                              Empty string deletes the range."
                    .into(),
                required: true,
            },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    path: String,
    lines: String,
    content: String,
}

fn parse_lines(spec: &str) -> Result<(usize, i64), String> {
    let trimmed = spec.trim();
    let (a, b) = trimmed
        .split_once(',')
        .ok_or_else(|| format!("lines must be `<start>,<end>` (got `{spec}`)"))?;
    let start: usize = a
        .trim()
        .parse()
        .map_err(|e| format!("invalid start in lines `{spec}`: {e}"))?;
    let end: i64 = b
        .trim()
        .parse()
        .map_err(|e| format!("invalid end in lines `{spec}`: {e}"))?;
    Ok((start, end))
}

#[async_trait]
impl crate::Tool for FileEditLines {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl FileEditLines {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let (start, end_signed) = match parse_lines(&input.lines) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(e),
        };

        if start < 1 {
            return ToolOutput::err("start line must be >= 1");
        }
        // end may be start - 1 to indicate pure insertion.
        let min_end = start as i64 - 1;
        if end_signed < min_end {
            return ToolOutput::err(format!(
                "end ({end_signed}) must be >= start - 1 ({min_end})"
            ));
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

        let uses_crlf = contents.contains("\r\n");
        let line_ending = if uses_crlf { "\r\n" } else { "\n" };

        // Compute byte offsets for line boundaries. A "line" here is the run
        // of characters up to (but not including) its terminating newline.
        // We track the start byte offset of each line, plus the offset just
        // past the file's last byte (sentinel for "line N+1").
        let line_starts = compute_line_starts(&contents);
        let total_lines = if contents.is_empty() {
            0
        } else {
            line_starts.len() - 1
        };

        // Allow start = total_lines + 1 (append).
        if start > total_lines + 1 {
            return ToolOutput::err(format!(
                "start ({start}) exceeds file length ({total_lines} lines, max start is {})",
                total_lines + 1
            ));
        }

        // Clamp end past EOF down to total_lines instead of erroring.
        let end_line_usize: usize = if end_signed < 0 {
            0
        } else {
            (end_signed as usize).min(total_lines)
        };

        // Byte range [range_start, range_end) covers the slice to replace.
        let is_insertion = end_line_usize + 1 == start;
        let range_start = if start <= total_lines {
            line_starts[start - 1]
        } else {
            // Append at EOF.
            contents.len()
        };
        let range_end = if is_insertion {
            range_start
        } else {
            // line_starts has total_lines + 1 entries (sentinel at end).
            line_starts[end_line_usize]
        };

        // Build replacement. Append a newline if content is non-empty and
        // doesn't already end with one — but only when the replacement
        // produces lines that should be terminated (i.e. the original range
        // ended with a newline, OR we're inserting/replacing in the middle).
        let mut replacement = input.content.clone();
        let original_range_ends_with_newline =
            range_end > range_start && contents.as_bytes()[range_end - 1] == b'\n';
        let needs_trailing_newline = !replacement.is_empty()
            && !replacement.ends_with('\n')
            && (original_range_ends_with_newline
                || range_end < contents.len()
                || is_insertion && range_start < contents.len());
        if needs_trailing_newline {
            replacement.push_str(line_ending);
        }

        // Normalize replacement line endings to match file style.
        if uses_crlf {
            replacement = lf_to_crlf(&replacement);
        }

        let mut new_contents =
            String::with_capacity(contents.len() - (range_end - range_start) + replacement.len());
        new_contents.push_str(&contents[..range_start]);
        new_contents.push_str(&replacement);
        new_contents.push_str(&contents[range_end..]);

        let removed = end_line_usize.saturating_sub(start.saturating_sub(1));
        let added = if input.content.is_empty() {
            0
        } else {
            input.content.lines().count().max(1)
        };
        let msg = if is_insertion {
            format!(
                "edited {}: inserted {added} line{} before line {start}",
                input.path,
                if added == 1 { "" } else { "s" }
            )
        } else if input.content.is_empty() {
            format!(
                "edited {}: deleted lines {start}-{end_line_usize} ({removed} line{})",
                input.path,
                if removed == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "edited {}: replaced lines {start}-{end_line_usize} ({removed} line{} → {added} line{})",
                input.path,
                if removed == 1 { "" } else { "s" },
                if added == 1 { "" } else { "s" }
            )
        };

        write_and_report(&self.ctx, &path, &input.path, &new_contents, msg).await
    }
}

/// Returns a vector with the byte offset of the start of each line, plus a
/// trailing sentinel equal to `contents.len()`. For an empty string, returns
/// `[]`.
fn compute_line_starts(contents: &str) -> Vec<usize> {
    if contents.is_empty() {
        return Vec::new();
    }
    let mut starts = Vec::with_capacity(64);
    starts.push(0);
    for (i, b) in contents.bytes().enumerate() {
        if b == b'\n' && i + 1 <= contents.len() {
            // Next line starts after the '\n'.
            if i + 1 < contents.len() {
                starts.push(i + 1);
            }
        }
    }
    starts.push(contents.len());
    starts
}

fn lf_to_crlf(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_basic() {
        let s = "a\nb\nc\n";
        let v = compute_line_starts(s);
        // offsets: line1=0, line2=2, line3=4, sentinel=6
        assert_eq!(v, vec![0, 2, 4, 6]);
    }

    #[test]
    fn line_starts_no_trailing_newline() {
        let s = "a\nb\nc";
        let v = compute_line_starts(s);
        assert_eq!(v, vec![0, 2, 4, 5]);
    }

    #[test]
    fn line_starts_empty() {
        let v = compute_line_starts("");
        assert!(v.is_empty());
    }
}
