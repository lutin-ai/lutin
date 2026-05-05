//! Ordered / unordered list — each item rendered as a bordered card with
//! an accent-tinted marker on the left and free-form body content on the
//! right. Matches the "Honest meta-observations" mockup.
//!
//! ```rust,ignore
//! use lutin_ui::widget::list;
//!
//! list::ordered().show(ui, items.len(), |i, ui| {
//!     ui.label(&items[i]);
//! });
//! ```

use egui::{FontId, Sense, Ui};

use crate::theme::theme;

#[derive(Clone, Debug)]
pub enum Marker {
    Ordered,
    Unordered,
    Custom(Vec<String>),
}

const MARKER_SIZE: f32 = 20.0;
const ITEM_PADDING_X: f32 = 16.0;
const ITEM_PADDING_LEFT: f32 = 14.0;
const ITEM_PADDING_Y: f32 = 12.0;
const ITEM_GAP: f32 = 10.0;
const MARKER_TEXT: f32 = 11.0;
const MARKER_GAP: f32 = 10.0;

pub struct List {
    marker: Marker,
}

impl List {
    pub fn ordered() -> Self {
        Self { marker: Marker::Ordered }
    }

    pub fn unordered() -> Self {
        Self { marker: Marker::Unordered }
    }

    pub fn custom(labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            marker: Marker::Custom(labels.into_iter().map(Into::into).collect()),
        }
    }

    pub fn show(
        self,
        ui: &mut Ui,
        num_items: usize,
        mut item_fn: impl FnMut(usize, &mut Ui),
    ) {
        for i in 0..num_items {
            if i > 0 {
                ui.add_space(ITEM_GAP);
            }
            self.draw_item(ui, i, &mut item_fn);
        }
    }

    fn marker_label(&self, idx: usize) -> Option<String> {
        match &self.marker {
            Marker::Ordered => Some(format!("{}", idx + 1)),
            Marker::Unordered => None,
            Marker::Custom(labels) => labels.get(idx).cloned(),
        }
    }

    fn draw_item(
        &self,
        ui: &mut Ui,
        idx: usize,
        item_fn: &mut dyn FnMut(usize, &mut Ui),
    ) {
        let t = theme();
        let frame = egui::Frame::new()
            .fill(t.surface.void)
            .stroke(egui::Stroke::new(1.0, t.border.default))
            .corner_radius(t.radii.md)
            .inner_margin(egui::Margin {
                left: ITEM_PADDING_LEFT as i8,
                right: ITEM_PADDING_X as i8,
                top: ITEM_PADDING_Y as i8,
                bottom: ITEM_PADDING_Y as i8,
            });

        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                draw_marker(ui, &self.marker_label(idx), &self.marker);
                ui.add_space(MARKER_GAP - ui.spacing().item_spacing.x);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    item_fn(idx, ui);
                });
            });
        });
    }
}

fn draw_marker(ui: &mut Ui, label: &Option<String>, marker: &Marker) {
    let t = theme();
    match marker {
        Marker::Unordered => {
            // Bullet: small filled accent dot, vertically aligned with first text line.
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(MARKER_SIZE, MARKER_SIZE),
                Sense::hover(),
            );
            let center = egui::pos2(rect.center().x, rect.min.y + 10.0);
            ui.painter().circle_filled(center, 3.0, t.accent.default);
        }
        _ => {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(MARKER_SIZE, MARKER_SIZE),
                Sense::hover(),
            );
            ui.painter().rect(
                rect,
                t.radii.sm,
                t.accent.glow_strong,
                egui::Stroke::new(1.0, t.accent.glow_strong),
                egui::StrokeKind::Inside,
            );
            if let Some(text) = label.as_deref() {
                let font = FontId::new(MARKER_TEXT, t.fonts.code.clone());
                let galley = ui.painter().layout_no_wrap(
                    text.to_string(),
                    font,
                    t.accent.default,
                );
                let pos = egui::pos2(
                    rect.center().x - galley.size().x / 2.0,
                    rect.center().y - galley.size().y / 2.0,
                );
                ui.painter().galley(pos, galley, t.accent.default);
            }
        }
    }
}
