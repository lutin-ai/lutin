//! Compact collapsed previews for file tools.
//!
//! Shown under the header row when a tool-call card is collapsed, so the user
//! gets a glimpse of the change / content without having to expand.

use egui::{Color32, Ui};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use crate::theme::theme;

const PREVIEW_LINES: usize = 3;
const LINE_MAX_CHARS: usize = 160;

/// Render a short preview for known file-mutation/read tools when collapsed.
/// Returns `true` if anything was drawn.
pub fn render(ui: &mut Ui, name: &str, args: &Value, output: Option<&str>) -> bool {
    match name {
        "file_edit" => render_edit(ui, args),
        "file_write" => render_write(ui, args),
        "file_read" => render_read(ui, output),
        _ => false,
    }
}

fn render_edit(ui: &mut Ui, args: &Value) -> bool {
    let (Some(old), Some(new)) = (
        args.get("old_string").and_then(|v| v.as_str()),
        args.get("new_string").and_then(|v| v.as_str()),
    ) else {
        return false;
    };

    let diff = TextDiff::from_lines(old, new);
    let mut lines: Vec<(char, Color32, String)> = Vec::new();
    let t = theme();
    for change in diff.iter_all_changes() {
        let (sign, color) = match change.tag() {
            ChangeTag::Delete => ('-', t.status.error.solid),
            ChangeTag::Insert => ('+', t.status.success.solid),
            ChangeTag::Equal => continue,
        };
        lines.push((sign, color, strip_newline(change.value()).to_owned()));
        if lines.len() >= PREVIEW_LINES {
            break;
        }
    }
    if lines.is_empty() {
        return false;
    }
    preview_frame(ui, &lines);
    true
}

fn render_write(ui: &mut Ui, args: &Value) -> bool {
    let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
        return false;
    };
    let t = theme();
    let lines: Vec<(char, Color32, String)> = content
        .lines()
        .take(PREVIEW_LINES)
        .map(|l| ('+', t.status.success.solid, l.to_owned()))
        .collect();
    if lines.is_empty() {
        return false;
    }
    preview_frame(ui, &lines);
    true
}

fn render_read(ui: &mut Ui, output: Option<&str>) -> bool {
    let Some(output) = output else {
        return false;
    };
    let t = theme();
    let lines: Vec<(char, Color32, String)> = output
        .lines()
        .take(PREVIEW_LINES)
        .map(|l| (' ', t.text.dim, l.to_owned()))
        .collect();
    if lines.is_empty() {
        return false;
    }
    preview_frame(ui, &lines);
    true
}

fn preview_frame(ui: &mut Ui, lines: &[(char, Color32, String)]) {
    let t = theme();
    ui.add_space(t.spacing.xs);
    let frame = egui::Frame::new()
        .fill(t.surface.abyss)
        .corner_radius(t.radii.sm)
        .inner_margin(egui::Margin {
            left: 6,
            right: 6,
            top: 3,
            bottom: 3,
        });
    frame.show(ui, |ui| {
        let w = ui.available_width();
        ui.set_width(w);
        ui.set_max_width(w);
        for (sign, color, line) in lines {
            let truncated = truncate(line, LINE_MAX_CHARS);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    egui::RichText::new(sign.to_string())
                        .size(11.5)
                        .color(*color)
                        .family(t.fonts.code.clone()),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(truncated)
                            .size(11.5)
                            .color(*color)
                            .family(t.fonts.code.clone()),
                    )
                    .truncate(),
                );
            });
        }
    });
}

fn strip_newline(s: &str) -> &str {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.strip_suffix('\r').unwrap_or(s)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}
