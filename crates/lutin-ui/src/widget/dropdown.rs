//! Themed dropdown / select widget.
//!
//! Renders a custom trigger button that opens a popup with selectable items.
//! State (popup open/closed) lives inside `egui::Memory`, keyed by the trigger's
//! persistent `Id` — caller only owns the selection value.
//!
//! Two trigger styles:
//! - [`show`] — boxed input-like trigger (bordered, filled).
//! - [`show_transparent`] — chromeless trigger for toolbars / inline use.
//!
//! Items are rendered with [`item`], which mutates the caller's selection in
//! place. For the common case of picking from a static list of `(value, label)`
//! pairs, use the generic [`dropdown`] convenience.

use egui::{FontFamily, FontId, Response, Sense, Ui};

use crate::theme::theme;

/// Boxed trigger: bordered, filled like an input. Caller fills the popup body.
pub fn show(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    selected_text: &str,
    width: f32,
    add_items: impl FnOnce(&mut Ui),
) -> Response {
    let t = theme();
    let id = ui.make_persistent_id(id);
    let padding = egui::vec2(t.spacing.md, t.spacing.sm);
    let rounding = t.radii.md;

    let frame_resp = egui::Frame::new()
        .fill(t.input.bg)
        .stroke(egui::Stroke::new(1.0, t.input.border))
        .corner_radius(rounding)
        .inner_margin(padding)
        .show(ui, |ui| {
            ui.set_width(width - padding.x * 2.0 - 2.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(selected_text)
                        .size(14.0)
                        .color(t.input.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("v")
                            .family(FontFamily::Proportional)
                            .size(14.0)
                            .color(t.text.dim),
                    );
                });
            });
        });

    let button_response = ui.interact(frame_resp.response.rect, id, Sense::click());
    if button_response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let popup_frame = egui::Frame::new()
        .fill(t.surface.elevated)
        .stroke(egui::Stroke::new(1.0, t.border.default))
        .corner_radius(t.radii.md)
        .inner_margin(t.spacing.xs);

    egui::Popup::from_toggle_button_response(&button_response)
        .width(width)
        .frame(popup_frame)
        .show(add_items);

    button_response
}

/// Chromeless trigger: no background, no border. For toolbars / inline use.
pub fn show_transparent(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    selected_text: &str,
    width: f32,
    add_items: impl FnOnce(&mut Ui),
) -> Response {
    show_transparent_with_icon(ui, id, None, selected_text, width, add_items)
}

/// Like [`show_transparent`] with an optional leading Material Symbols glyph.
pub fn show_transparent_with_icon(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    leading_icon: Option<&str>,
    selected_text: &str,
    width: f32,
    add_items: impl FnOnce(&mut Ui),
) -> Response {
    let t = theme();
    let id = ui.make_persistent_id(id);
    let padding = egui::vec2(t.spacing.sm, t.spacing.xs);

    let frame_resp = egui::Frame::new()
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::NONE)
        .inner_margin(padding)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                if let Some(ico) = leading_icon {
                    // Cheap clone: FontFamily::Name(Arc<str>) — Arc bump, and
                    // RichText::family takes FontFamily by value.
                    ui.label(
                        egui::RichText::new(ico)
                            .family(t.fonts.icon.clone())
                            .size(14.0)
                            .color(t.input.text),
                    );
                }
                ui.label(
                    egui::RichText::new(selected_text)
                        .size(14.0)
                        .color(t.input.text),
                );
                ui.label(
                    egui::RichText::new("v")
                        .family(FontFamily::Proportional)
                        .size(14.0)
                        .color(t.text.dim),
                );
            });
        });

    let button_response = ui.interact(frame_resp.response.rect, id, Sense::click());
    if button_response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let popup_frame = egui::Frame::new()
        .fill(t.surface.elevated)
        .stroke(egui::Stroke::new(1.0, t.border.default))
        .corner_radius(t.radii.md)
        .inner_margin(t.spacing.xs);

    egui::Popup::from_toggle_button_response(&button_response)
        .width(width)
        .frame(popup_frame)
        .show(add_items);

    button_response
}

/// String-specialised [`item`]: avoids cloning the caller's `&str` into a
/// `String` just to satisfy the generic `&T` bound. Only allocates on click.
pub fn item_str(ui: &mut Ui, current: &mut String, value: &str, label: &str) -> bool {
    let t = theme();
    let selected = current.as_str() == value;

    let desired_size = egui::vec2(ui.available_width(), 32.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    if ui.is_rect_visible(rect) {
        let fill = if selected || response.hovered() {
            t.surface.raised
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, t.radii.sm, fill);

        let text_pos = egui::pos2(rect.min.x + t.spacing.md, rect.center().y);
        let font = FontId::new(14.0, FontFamily::Proportional);
        let color = if selected || response.hovered() {
            t.text.bright
        } else {
            t.text.default
        };
        ui.painter()
            .text(text_pos, egui::Align2::LEFT_CENTER, label, font, color);
    }

    if response.clicked() && !selected {
        *current = value.to_owned();
        ui.close();
        return true;
    }
    false
}

/// Selectable item for use inside a dropdown popup body. Returns `true` on the
/// frame the user picks this item (selection mutates in place, popup closes).
pub fn item<T: PartialEq + Clone>(
    ui: &mut Ui,
    current: &mut T,
    value: &T,
    label: &str,
) -> bool {
    let t = theme();
    let selected = current == value;

    let desired_size = egui::vec2(ui.available_width(), 32.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    if ui.is_rect_visible(rect) {
        let fill = if selected || response.hovered() {
            t.surface.raised
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, t.radii.sm, fill);

        let text_pos = egui::pos2(rect.min.x + t.spacing.md, rect.center().y);
        let font = FontId::new(14.0, FontFamily::Proportional);
        let color = if selected || response.hovered() {
            t.text.bright
        } else {
            t.text.default
        };
        ui.painter()
            .text(text_pos, egui::Align2::LEFT_CENTER, label, font, color);
    }

    if response.clicked() && !selected {
        *current = value.clone();
        ui.close();
        return true;
    }
    false
}

/// Convenience: full dropdown over a static `(value, label)` slice. Picks the
/// label of the currently-selected value for the trigger text.
///
/// If `selected` is not present in `options`, this is a caller bug: in debug
/// builds it trips a `debug_assert!`; in release it falls back to displaying
/// `"<unknown>"` so the mistake is visible rather than rendering blank.
/// Returns the trigger `Response`.
pub fn dropdown<T: Clone + PartialEq>(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    selected: &mut T,
    options: &[(T, &str)],
    width: f32,
) -> Response {
    let label = options
        .iter()
        .find_map(|(v, l)| (v == selected).then_some(*l))
        .unwrap_or_else(|| {
            debug_assert!(
                false,
                "dropdown selected value not present in options (len={})",
                options.len()
            );
            "<unknown>"
        });
    show(ui, id, label, width, |ui| {
        for (value, label) in options {
            item(ui, selected, value, label);
        }
    })
}
