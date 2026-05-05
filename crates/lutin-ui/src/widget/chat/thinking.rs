//! Collapsible reasoning / thinking block.
//!
//! Streaming: shows the partial text as plain mono, animated indicator dot,
//! defaults expanded. Complete: renders text as markdown, defaults collapsed.
//!
//! Expand state is persisted in egui memory keyed by `id_salt`, so reusing the
//! same id across frames preserves the user's toggle.

use egui::Ui;

use crate::markdown::Markdown;
use crate::theme::theme;
use crate::widget::{divider, icon};

/// Render a thinking block. Pass a stable `id_salt` (e.g. message id) so the
/// expand state survives across frames.
pub fn show(ui: &mut Ui, id_salt: &str, text: &str, streaming: bool) {
    let t = theme();

    let frame = egui::Frame::new()
        .fill(t.surface.deep)
        .stroke(egui::Stroke::new(1.0, t.border.subtle))
        .corner_radius(t.radii.md)
        .inner_margin(egui::vec2(t.spacing.md, t.spacing.sm));

    frame.show(ui, |ui| {
        let w = ui.available_width();
        ui.set_width(w);
        ui.set_max_width(w);

        let id = ui.auto_id_with(("thinking_block", id_salt));
        let default_expanded = streaming;
        let mut expanded =
            ui.memory(|mem| mem.data.get_temp::<bool>(id).unwrap_or(default_expanded));

        let header = ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(icon::ICON_PSYCHOLOGY)
                    .family(t.fonts.icon.clone())
                    .size(16.0)
                    .color(t.text.dim),
            );
            ui.add_space(t.spacing.xs);

            let label = if streaming { "Thinking…" } else { "Thinking" };
            ui.label(
                egui::RichText::new(label)
                    .size(13.0)
                    .color(t.text.dim)
                    .italics(),
            );

            if streaming {
                let phase = ui.ctx().input(|i| i.time) * 3.0;
                let alpha = ((phase.sin() + 1.0) / 2.0 * 200.0 + 55.0) as u8;
                ui.label(
                    egui::RichText::new("\u{25CF}")
                        .size(8.0)
                        .color(t.text.dim.gamma_multiply(f32::from(alpha) / 255.0)),
                );
                ui.ctx().request_repaint();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let chevron = if expanded {
                    icon::ICON_EXPAND_LESS
                } else {
                    icon::ICON_EXPAND_MORE
                };
                ui.label(
                    egui::RichText::new(chevron)
                        .family(t.fonts.icon.clone())
                        .size(18.0)
                        .color(t.text.dim),
                );
            });
        });

        if header.response.interact(egui::Sense::click()).clicked() {
            expanded = !expanded;
            ui.memory_mut(|mem| mem.data.insert_temp(id, expanded));
        }

        if expanded {
            ui.add_space(t.spacing.xs);
            divider::horizontal(ui);
            ui.add_space(t.spacing.xs);
            if streaming {
                ui.label(egui::RichText::new(text).size(13.0).color(t.text.dim));
            } else {
                Markdown::new(text).show(ui);
            }
        }
    });
}
