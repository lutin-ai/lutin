//! Inline keyboard-shortcut hint (e.g. `⌘K`). Mono 11px on `bg_2` with a
//! `line_strong` border. Per design doc 17.

use egui::{FontId, Response, Sense, Ui, Widget};

use crate::theme::theme;

const TEXT_SIZE: f32 = 11.0;
const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 2.0;

pub struct Kbd {
    text: egui::WidgetText,
}

impl Kbd {
    pub fn new(text: impl Into<egui::WidgetText>) -> Self {
        Self { text: text.into() }
    }
}

impl Widget for Kbd {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = theme();
        let font = FontId::new(TEXT_SIZE, t.fonts.code.clone());
        let padding = egui::vec2(PAD_X, PAD_Y);

        let wrap_width = ui.available_width() - padding.x * 2.0;
        let galley = self
            .text
            .into_galley(ui, Some(egui::TextWrapMode::Extend), wrap_width, font);

        let desired_size = padding * 2.0 + galley.size();
        let (rect, response) = ui.allocate_at_least(desired_size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let stroke = egui::Stroke::new(1.0, t.border.strong);
            ui.painter().rect(
                rect,
                t.radii.sm,
                t.surface.elevated,
                stroke,
                egui::StrokeKind::Inside,
            );
            let text_pos = egui::pos2(
                rect.min.x + padding.x,
                rect.center().y - galley.size().y / 2.0,
            );
            ui.painter().galley(text_pos, galley, t.text.dim);
        }

        response
    }
}

pub fn kbd(text: impl Into<egui::WidgetText>) -> Kbd {
    Kbd::new(text)
}
