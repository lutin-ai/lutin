//! Custom body renderers for file-mutation tools.
//!
//! `file_edit` → unified-diff view between `old_string` and `new_string`.
//! `write_file` → path header + content preview treated as additions.

use egui::{Color32, Ui};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use crate::theme::theme;

/// Renderer for the `file_edit` tool.
pub fn render_file_edit(ui: &mut Ui, args: &Value) -> bool {
    let Some(old) = args.get("old_string").and_then(Value::as_str) else {
        return false;
    };
    let Some(new) = args.get("new_string").and_then(Value::as_str) else {
        return false;
    };
    let path = args.get("path").and_then(Value::as_str).unwrap_or("");

    path_header(ui, path);
    diff_block(ui, old, new);
    true
}

/// Renderer for the `write_file` tool.
pub fn render_write_file(ui: &mut Ui, args: &Value) -> bool {
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return false;
    };
    let path = args.get("path").and_then(Value::as_str).unwrap_or("");

    path_header(ui, path);
    // Treat the whole file as an addition.
    diff_block(ui, "", content);
    true
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn path_header(ui: &mut Ui, path: &str) {
    if path.is_empty() {
        return;
    }
    let t = theme();
    ui.label(
        egui::RichText::new(path)
            .size(12.0)
            .color(t.text.muted)
            .family(t.fonts.code.clone()),
    );
    ui.add_space(t.spacing.xs);
}

fn diff_block(ui: &mut Ui, old: &str, new: &str) {
    let t = theme();

    let frame = egui::Frame::new()
        .fill(t.surface.abyss)
        .corner_radius(t.radii.sm)
        .inner_margin(t.spacing.xs);

    frame.show(ui, |ui| {
        let w = ui.available_width();
        ui.set_width(w);
        ui.set_max_width(w);

        let diff = TextDiff::from_lines(old, new);
        for change in diff.iter_all_changes() {
            let (sign, row_bg, text_color) = match change.tag() {
                ChangeTag::Delete => ('-', tint(t.status.error.solid, 0.18), t.status.error.solid),
                ChangeTag::Insert => ('+', tint(t.status.success.solid, 0.18), t.status.success.solid),
                ChangeTag::Equal => (' ', Color32::TRANSPARENT, t.text.dim),
            };
            let line = strip_newline(change.value());
            diff_line(ui, sign, line, row_bg, text_color);
        }
    });
}

fn diff_line(ui: &mut Ui, sign: char, line: &str, bg: Color32, fg: Color32) {
    let t = theme();
    let row = egui::Frame::new()
        .fill(bg)
        .inner_margin(egui::Margin {
            left: 4,
            right: 4,
            top: 1,
            bottom: 1,
        });
    row.show(ui, |ui| {
        let w = ui.available_width();
        ui.set_width(w);
        ui.set_max_width(w);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                egui::RichText::new(sign.to_string())
                    .size(12.0)
                    .color(fg)
                    .family(t.fonts.code.clone()),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(line)
                        .size(12.0)
                        .color(fg)
                        .family(t.fonts.code.clone()),
                )
                .wrap(),
            );
        });
    });
}

fn strip_newline(s: &str) -> &str {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.strip_suffix('\r').unwrap_or(s)
}

/// Blend a color toward transparent for a soft row background.
fn tint(c: Color32, alpha: f32) -> Color32 {
    let a = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}
