//! Activity-rail timeline. Per design doc 17 §"Timeline entry".
//!
//! Each entry is three rows anchored to a 1px vertical line drawn at
//! `x = 24px` from the rail's left edge. The dot encodes event kind.
//!
//! ```rust,ignore
//! use lutin_ui::widget::timeline::{Timeline, EventKind, Entry};
//! Timeline::new().show(ui, &[
//!     Entry { kind: EventKind::Spawned, event: "SPAWNED", agent: "strat-1",
//!             detail: "kicked off /design", time: "19:23:04 · 12s ago" },
//! ]);
//! ```

use egui::{FontId, Sense, Stroke, Ui};

use crate::theme::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Spawned,
    Finished,
    Failed,
    Default,
}

pub struct Entry<'a> {
    pub kind: EventKind,
    pub event: &'a str,
    pub agent: &'a str,
    pub detail: &'a str,
    pub time: &'a str,
}

const RAIL_X: f32 = 24.0;
const DOT_RADIUS: f32 = 4.0;
const ENTRY_GAP: f32 = 14.0;
const ROW1_TEXT: f32 = 12.5;
const ROW2_TEXT: f32 = 11.5;
const ROW3_TEXT: f32 = 10.5;
const ROW_LINE_HEIGHT: f32 = 17.0;

pub struct Timeline;

impl Timeline {
    pub fn new() -> Self {
        Self
    }

    pub fn show(self, ui: &mut Ui, entries: &[Entry<'_>]) {
        if entries.is_empty() {
            return;
        }
        let t = theme();
        let entry_height = ROW_LINE_HEIGHT * 3.0 + ENTRY_GAP;
        let total_height = entry_height * entries.len() as f32;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), total_height),
            Sense::hover(),
        );

        // Vertical rail line
        let rail_x = rect.min.x + RAIL_X;
        ui.painter().vline(
            rail_x,
            rect.y_range(),
            Stroke::new(1.0, t.border.subtle),
        );

        for (i, entry) in entries.iter().enumerate() {
            let top = rect.min.y + entry_height * i as f32;
            draw_entry(ui, entry, egui::pos2(rect.min.x, top), rect.max.x - rect.min.x);
        }
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

fn draw_entry(ui: &mut Ui, entry: &Entry<'_>, top_left: egui::Pos2, width: f32) {
    let t = theme();
    let rail_x = top_left.x + RAIL_X;
    let row1_y = top_left.y;

    draw_dot(ui, egui::pos2(rail_x, row1_y + ROW_LINE_HEIGHT * 0.5), entry.kind);

    let text_x = rail_x + 14.0;
    let text_max_x = top_left.x + width;
    let text_w = (text_max_x - text_x).max(0.0);

    // Row 1: EVENT (uppercase, tone) + agent (bright)
    let event_color = event_tone_color(&t, entry.kind);
    let event_text = egui::RichText::new(entry.event.to_uppercase())
        .font(FontId::new(ROW3_TEXT, t.fonts.text.clone()))
        .color(event_color)
        .strong();
    let agent_text = egui::RichText::new(entry.agent)
        .font(FontId::new(ROW1_TEXT, t.fonts.text.clone()))
        .color(t.text.bright);

    let mut row1 = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                egui::pos2(text_x, row1_y),
                egui::vec2(text_w, ROW_LINE_HEIGHT),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row1.label(event_text);
    row1.label(agent_text);

    // Row 2: secondary detail
    let mut row2 = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                egui::pos2(text_x, row1_y + ROW_LINE_HEIGHT),
                egui::vec2(text_w, ROW_LINE_HEIGHT),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row2.label(
        egui::RichText::new(entry.detail)
            .font(FontId::new(ROW2_TEXT, t.fonts.text.clone()))
            .color(t.text.dim),
    );

    // Row 3: timestamp (mono)
    let mut row3 = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                egui::pos2(text_x, row1_y + ROW_LINE_HEIGHT * 2.0),
                egui::vec2(text_w, ROW_LINE_HEIGHT),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row3.label(
        egui::RichText::new(entry.time)
            .font(FontId::new(ROW3_TEXT, t.fonts.code.clone()))
            .color(t.text.muted),
    );
}

fn draw_dot(ui: &Ui, center: egui::Pos2, kind: EventKind) {
    let t = theme();
    let p = ui.painter();
    match kind {
        EventKind::Spawned => {
            // Filled accent dot with translucent halo.
            p.circle_filled(center, DOT_RADIUS + 4.0, t.accent.glow_strong);
            p.circle_filled(center, DOT_RADIUS, t.accent.default);
        }
        EventKind::Finished => {
            p.circle_filled(center, DOT_RADIUS, t.surface.void);
            p.circle_stroke(center, DOT_RADIUS, Stroke::new(1.5, t.status.success.solid));
        }
        EventKind::Failed => {
            p.circle_filled(center, DOT_RADIUS, t.surface.void);
            p.circle_stroke(center, DOT_RADIUS, Stroke::new(1.5, t.status.error.solid));
        }
        EventKind::Default => {
            p.circle_filled(center, DOT_RADIUS, t.surface.void);
            p.circle_stroke(center, DOT_RADIUS, Stroke::new(1.0, t.text.muted));
        }
    }
}

fn event_tone_color(t: &crate::theme::Theme, kind: EventKind) -> egui::Color32 {
    match kind {
        EventKind::Spawned => t.accent.bright,
        EventKind::Finished => t.status.success.solid,
        EventKind::Failed => t.status.error.solid,
        EventKind::Default => t.text.dim,
    }
}
