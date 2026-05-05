//! Themed container with optional header strip + body. Builder API; body
//! supplied by closure. Use for settings sections, sidebar groups, info panes.

use egui::{Color32, InnerResponse, Ui};

use crate::theme::theme;

pub struct Panel {
    title: Option<egui::WidgetText>,
    fill: Option<Color32>,
    padding: Option<f32>,
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel {
    pub fn new() -> Self {
        Self {
            title: None,
            fill: None,
            padding: None,
        }
    }

    pub fn header(mut self, title: impl Into<egui::WidgetText>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn fill(mut self, fill: Color32) -> Self {
        self.fill = Some(fill);
        self
    }

    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = Some(padding);
        self
    }

    pub fn show<R>(
        self,
        ui: &mut Ui,
        body: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<R> {
        let t = theme();
        let bg = self.fill.unwrap_or(t.panel.bg);
        let pad = self.padding.unwrap_or(t.spacing.md);
        let radius = t.radii.md;
        let stroke = if t.panel.border == Color32::TRANSPARENT {
            egui::Stroke::NONE
        } else {
            egui::Stroke::new(1.0, t.panel.border)
        };

        egui::Frame::new()
            .fill(bg)
            .stroke(stroke)
            .corner_radius(radius)
            .show(ui, |ui| {
                if let Some(title) = self.title {
                    egui::Frame::new()
                        .fill(t.panel.header_bg)
                        .inner_margin(egui::Margin::symmetric(pad as i8, (t.spacing.sm) as i8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(title);
                        });
                }

                egui::Frame::new()
                    .inner_margin(pad as i8)
                    .show(ui, |ui| body(ui))
                    .inner
            })
    }
}
