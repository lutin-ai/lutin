//! Error displays — inline (sits inside another container) and block (its
//! own framed surface).

use egui::Ui;

use crate::theme::theme;
use crate::widget::icon;

/// Inline error text with leading icon. No outer frame — pair with whatever
/// surface the error sits inside.
pub fn show_inline(ui: &mut Ui, message: &str) {
    let t = theme();
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(icon::ICON_ERROR)
                .family(t.fonts.icon.clone())
                .size(16.0)
                .color(t.status.error.solid),
        );
        ui.add_space(t.spacing.xs);
        ui.add(
            egui::Label::new(
                egui::RichText::new(message)
                    .size(14.0)
                    .color(t.status.error.solid),
            )
            .wrap(),
        );
    });
}

/// Standalone error block — framed in error-tinted surface. For top-level
/// system errors that aren't attached to another container.
pub fn show_block(ui: &mut Ui, message: &str) {
    let t = theme();

    let frame = egui::Frame::new()
        .fill(t.status.error.dim)
        .stroke(egui::Stroke::new(1.0, t.status.error.border))
        .corner_radius(t.radii.md)
        .inner_margin(egui::vec2(t.spacing.md, t.spacing.sm));

    frame.show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(icon::ICON_ERROR)
                    .family(t.fonts.icon.clone())
                    .size(16.0)
                    .color(t.status.error.solid),
            );
            ui.add_space(t.spacing.xs);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(message)
                        .size(14.0)
                        .color(t.status.error.solid),
                )
                .wrap(),
            );
        });
    });
}
