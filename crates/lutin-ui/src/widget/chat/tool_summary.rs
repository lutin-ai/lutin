//! Header-inline summaries for tool calls.
//!
//! Produces a short preview string derived from the tool's primary argument,
//! so a collapsed tool-call card reads like `read_file  src/main.rs` rather
//! than just `read_file`. Recognises a fixed set of common tool names; pass
//! an unknown name and you get `None`.

use egui::Ui;
use serde_json::Value;

use super::tool_diff;

const MAX_SUMMARY_LEN: usize = 80;

pub type BuiltinToolRenderer = fn(ui: &mut Ui, args: &Value) -> bool;

/// Compact one-line summary for a known tool. `None` = no preview available.
pub fn summary(name: &str, args: &Value) -> Option<String> {
    let s = match name {
        "file_read" | "file_write" | "file_edit" | "image_view" => str_arg(args, "path")?,
        "shell" => first_line(str_arg(args, "command")?),
        "file_grep" => {
            let pat = str_arg(args, "pattern")?;
            match str_arg(args, "path") {
                Some(p) if !p.is_empty() && p != "." => format!("{pat}  in {p}"),
                _ => pat,
            }
        }
        "file_glob" => {
            let pat = str_arg(args, "pattern")?;
            match str_arg(args, "path") {
                Some(p) if !p.is_empty() && p != "." => format!("{pat}  in {p}"),
                _ => pat,
            }
        }
        "file_list" | "file_tree" => str_arg(args, "path").unwrap_or_else(|| ".".into()),
        "http_request" => {
            let url = str_arg(args, "url")?;
            let method = str_arg(args, "method").unwrap_or_else(|| "GET".into());
            format!("{} {url}", method.to_uppercase())
        }
        "screenshot_url" => str_arg(args, "url")?,
        "web_search" => str_arg(args, "query")?,
        "wait" => format!("{}s", args.get("seconds").and_then(Value::as_i64).unwrap_or(0)),
        "spawn_agent" => str_arg(args, "name")?,
        "message_agent" | "get_agent" | "stop_agent" => str_arg(args, "agent_name")?,
        "load_skill" | "unload_skill" => str_arg(args, "name")?,
        "start_workflow" => str_arg(args, "workflow_name")?,
        "show_widget" => str_arg(args, "widget")?,
        "close_widget" => str_arg(args, "widget_id")?,
        "new_chat" => str_arg(args, "title")
            .or_else(|| str_arg(args, "handoff").map(first_line))
            .unwrap_or_else(|| "new chat".into()),
        _ => return None,
    };
    Some(truncate(&s, MAX_SUMMARY_LEN))
}

/// Custom body renderer for a tool, if one exists. Currently `file_edit` and
/// `file_write` get diff renderers.
pub fn pick_renderer(name: &str) -> Option<BuiltinToolRenderer> {
    match name {
        "file_edit" => Some(tool_diff::render_file_edit),
        "file_write" => Some(tool_diff::render_write_file),
        _ => None,
    }
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn first_line(s: String) -> String {
    s.lines().next().unwrap_or("").to_owned()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}\u{2026}")
}
